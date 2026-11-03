//! Exact outgoing entitlement must not rebuild the channel history per check.
use super::*;
use crate::{eventlog::Author, identity::device::DeviceIdentity};
use std::sync::Arc;

fn node(path: &std::path::Path) -> Arc<Node> {
    std::fs::create_dir_all(path).unwrap();
    let (dm, _) = tokio::sync::mpsc::unbounded_channel();
    let (group, _) = tokio::sync::mpsc::unbounded_channel();
    let (files, _) = tokio::sync::mpsc::unbounded_channel();
    Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(crate::discovery::Roster::default())),
        dm,
        group,
        files,
        &path.join("events.log"),
        &path.join("sent.log"),
        "pw",
    )
    .unwrap()
}

fn append_history(node: &Node, channel: ConversationId, count: usize) -> EventId {
    let author = Author::from_ed25519(node.identity.public().ed25519_pub);
    let mut log = node.log.lock().unwrap();
    let mut first = None;
    for _ in 0..count {
        let (parents, lamport) = log.prepare(&channel).unwrap();
        let sequence = log.next_sequence(&channel, &author).unwrap();
        let event = Event::new(
            &node.identity,
            channel,
            sequence,
            parents,
            lamport,
            1,
            EventKind::Message,
            vec![7; 8192],
        );
        first.get_or_insert(event.id);
        log.append(event).unwrap();
    }
    first.unwrap()
}

fn settle_contains(
    node: &Node,
    channel: ConversationId,
    event: EventId,
    peer: &PublicIdentity,
) -> bool {
    for _ in 0..4096 {
        match node.group_file_outgoing_contains(channel, event, peer) {
            Ok(allowed) => return allowed,
            Err(LogError::StaleCursor) => {}
            Err(error) => panic!("outgoing entitlement preparation failed: {error}"),
        }
    }
    panic!("exact outgoing entitlement did not finish bounded preparation")
}

#[tokio::test]
async fn group_file_exact_outgoing_warm_checks_do_not_hydrate_32_or_128_messages() {
    for count in [32, 128] {
        let root = tempfile::tempdir().unwrap();
        let node = node(root.path());
        let peer = DeviceIdentity::generate();
        let channel = node
            .create_channel("File custody", vec![peer.public()])
            .await
            .unwrap();
        let event = append_history(&node, channel, count);
        crate::node::test_support::settle_group_metadata(&node, channel);
        assert!(settle_contains(&node, channel, event, &peer.public()));
        let before = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        for _ in 0..3 {
            assert!(
                node.group_file_outgoing_contains(channel, event, &peer.public())
                    .unwrap()
            );
        }
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            before,
            "warm exact outgoing checks must not re-read {count} Message bodies"
        );
    }
}

#[tokio::test]
async fn group_file_exact_outgoing_revalidates_unapplied_leave() {
    let root = tempfile::tempdir().unwrap();
    let node = node(root.path());
    let peer = DeviceIdentity::generate();
    let channel = node
        .create_channel("File custody", vec![peer.public()])
        .await
        .unwrap();
    let event = append_history(&node, channel, 128);
    crate::node::test_support::settle_group_metadata(&node, channel);
    assert!(settle_contains(&node, channel, event, &peer.public()));
    let leave = {
        let log = node.log.lock().unwrap();
        let (parents, lamport) = log.prepare(&channel).unwrap();
        Event::new(
            &peer,
            channel,
            1,
            parents,
            lamport,
            1,
            EventKind::ChannelLeave,
            vec![],
        )
    };
    node.log.lock().unwrap().append(leave).unwrap();
    crate::node::test_support::settle_group_metadata(&node, channel);
    let oracle = node
        .sync_store(&peer.public())
        .lock()
        .unwrap()
        .authorized_event_ids(&channel)
        .unwrap()
        .contains(&event);
    assert_eq!(
        settle_contains(&node, channel, event, &peer.public()),
        oracle,
        "terminal closing ancestry keeps the existing historical entitlement"
    );
    let after_leave = append_history(&node, channel, 1);
    assert!(
        !matches!(
            node.group_file_outgoing_contains(channel, after_leave, &peer.public()),
            Ok(true)
        ),
        "a new event outside the terminal ancestry must not inherit an old positive grant"
    );
    crate::node::test_support::settle_group_metadata(&node, channel);
    assert!(!settle_contains(
        &node,
        channel,
        after_leave,
        &peer.public()
    ));
}

#[tokio::test]
async fn group_file_exact_outgoing_retains_at_most_four_prepared_recipient_views() {
    let root = tempfile::tempdir().unwrap();
    let node = node(root.path());
    let peers: Vec<_> = (0..5).map(|_| DeviceIdentity::generate()).collect();
    let channel = node
        .create_channel(
            "File custody",
            peers.iter().map(DeviceIdentity::public).collect(),
        )
        .await
        .unwrap();
    let event = append_history(&node, channel, 32);
    crate::node::test_support::settle_group_metadata(&node, channel);
    for peer in &peers {
        assert!(settle_contains(&node, channel, event, &peer.public()));
        assert!(node.group_file_delivery.authorization.lock().unwrap().len() <= 4);
    }
    assert_eq!(
        node.group_file_delivery.authorization.lock().unwrap().len(),
        4
    );
    assert!(
        settle_contains(&node, channel, event, &peers[0].public()),
        "an evicted scope can prepare again without changing entitlement"
    );
    assert_eq!(
        node.group_file_delivery.authorization.lock().unwrap().len(),
        4
    );
}
