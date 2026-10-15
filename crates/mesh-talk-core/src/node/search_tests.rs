//! Search regressions live beside the search implementation so unrelated node
//! features can evolve without editing the large shared node test file.

use super::*;
use crate::discovery::announce::Announce;
use crate::discovery::roster::Roster;
use crate::identity::device::DeviceIdentity;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

#[tokio::test]
async fn search_finds_a_sent_dm_by_keyword() {
    let me = DeviceIdentity::generate();
    let peer = DeviceIdentity::generate();
    let dir = tempfile::tempdir().unwrap();
    let roster = Arc::new(Mutex::new(Roster::default()));
    roster.lock().unwrap().update(
        &Announce::new(&peer, "Peer", 1),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &me.public().user_id(),
    );
    let account = crate::identity::account::Account::generate();
    roster.lock().unwrap().update(
        &Announce::new_with_account(&peer, &account, "Peer", 1),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &me.public().user_id(),
    );
    let original_roster = roster.clone();
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
    assert_eq!(
        hits[0].account_id.as_deref(),
        Some(account.account_id().as_str())
    );
    original_roster.lock().unwrap().evict_stale(Duration::ZERO);
    assert!(original_roster.lock().unwrap().peers().is_empty());
    let offline_hits = node.search("NOON");
    assert_eq!(offline_hits.len(), 1);
    assert_eq!(offline_hits[0].target, peer_uid);
    assert_eq!(
        offline_hits[0].account_id.as_deref(),
        Some(account.account_id().as_str())
    );
    original_roster.lock().unwrap().update(
        &Announce::new_post_office_with_account(&peer, &account, "Relay", 1),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &node.user_id(),
    );
    assert!(node.search("NOON").is_empty());
    assert!(node.search("   ").is_empty());
    assert!(node.search("absent-keyword").is_empty());
}

#[test]
fn search_finds_account_message_after_contact_goes_offline() {
    use crate::node::DmEnvelope;
    use crate::node::conversation::account_conversation_id;

    let dir = tempfile::tempdir().unwrap();
    let (dm, _dm_rx) = mpsc::unbounded_channel();
    let (ch, _ch_rx) = mpsc::unbounded_channel();
    let (file, _file_rx) = mpsc::unbounded_channel();
    let node = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        ch,
        file,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let own = node.account_id();
    let contact = "a".repeat(32);
    let conversation = account_conversation_id(&own, &contact);
    let message = DmEnvelope::new(
        own,
        contact.clone(),
        [7; 32],
        MessageBody::new(b"offline project notes".to_vec(), None).encode(),
    );
    node.sentlog
        .lock()
        .unwrap()
        .record(conversation, 1, 1, &message.encode())
        .unwrap();

    assert_eq!(node.known_account_ids(), vec![contact.clone()]);
    let hits = node.search("PROJECT");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].target, contact);
    assert_eq!(hits[0].text, b"offline project notes");
}

#[test]
fn search_does_not_present_a_relay_only_account_as_a_contact() {
    use crate::node::DmEnvelope;
    use crate::node::conversation::account_conversation_id;

    let dir = tempfile::tempdir().unwrap();
    let me = DeviceIdentity::generate();
    let relay = DeviceIdentity::generate();
    let relay_account = crate::identity::account::Account::generate();
    let roster = Arc::new(Mutex::new(Roster::default()));
    roster.lock().unwrap().update(
        &Announce::new_post_office_with_account(&relay, &relay_account, "Relay", 1),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &me.public().user_id(),
    );
    let (dm, _dm_rx) = mpsc::unbounded_channel();
    let (ch, _ch_rx) = mpsc::unbounded_channel();
    let (file, _file_rx) = mpsc::unbounded_channel();
    let node = Node::open(
        me,
        roster,
        dm,
        ch,
        file,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let own = node.account_id();
    let relay_id = relay_account.account_id();
    let message = DmEnvelope::new(
        own.clone(),
        relay_id.clone(),
        [8; 32],
        MessageBody::new(b"relay diagnostic token".to_vec(), None).encode(),
    );
    node.sentlog
        .lock()
        .unwrap()
        .record(
            account_conversation_id(&own, &relay_id),
            1,
            1,
            &message.encode(),
        )
        .unwrap();

    assert!(node.known_account_ids().contains(&relay_id));
    assert!(node.search("diagnostic").is_empty());
}

#[tokio::test]
async fn search_excludes_recalled_account_text() {
    use crate::eventlog::event::EventId;
    use crate::node::DmEnvelope;
    use crate::node::conversation::account_conversation_id;

    let dir = tempfile::tempdir().unwrap();
    let (dm, _) = mpsc::unbounded_channel();
    let (ch, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    let node = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        ch,
        file,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let own = node.account_id();
    let contact = "b".repeat(32);
    let conv = account_conversation_id(&own, &contact);
    let recalled = EventId::new([1; 32]);
    let now = crate::node::node::now_millis();
    for (seq, id, text) in [
        (1, recalled, b"withdrawn account text".as_slice()),
        (
            2,
            EventId::new([2; 32]),
            b"retained account text".as_slice(),
        ),
    ] {
        let message = DmEnvelope::new(
            own.clone(),
            contact.clone(),
            *id.as_bytes(),
            MessageBody::new(text.to_vec(), None).encode(),
        );
        node.sentlog
            .lock()
            .unwrap()
            .record(conv, seq, now, &message.encode())
            .unwrap();
    }
    assert_eq!(node.search("account text").len(), 2);
    node.recall_account(&contact, recalled).await.unwrap();
    assert!(node.search("withdrawn").is_empty());
    assert_eq!(node.search("retained").len(), 1);
}

#[tokio::test]
async fn search_excludes_recalled_channel_text() {
    let dir = tempfile::tempdir().unwrap();
    let (dm, _) = mpsc::unbounded_channel();
    let (ch, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    let node = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        ch,
        file,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let channel = node.create_channel("project", vec![]).await.unwrap();
    node.send_channel_message(channel, b"withdrawn channel text")
        .await
        .unwrap();
    node.send_channel_message(channel, b"retained channel text")
        .await
        .unwrap();
    let recalled = node
        .channel_history(channel, 10)
        .into_iter()
        .find(|entry| entry.text == b"withdrawn channel text")
        .unwrap()
        .id;
    assert_eq!(node.search("channel text").len(), 2);
    node.recall_channel(channel, recalled).await.unwrap();
    assert!(node.search("withdrawn").is_empty());
    assert_eq!(node.search("retained").len(), 1);
}
