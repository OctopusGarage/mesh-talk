//! Admission queries use current signed membership without hydrating messages.
use super::*;
use crate::eventlog::{Author, Event, EventKind};

fn append_messages(node: &Node, channel: crate::eventlog::ConversationId, count: usize) {
    let author = Author::from_ed25519(node.identity.public().ed25519_pub);
    let mut log = node.log.lock().unwrap();
    for _ in 0..count {
        let (parents, lamport) = log.prepare(&channel).unwrap();
        let sequence = log.next_sequence(&channel, &author).unwrap();
        log.append(Event::new(
            &node.identity,
            channel,
            sequence,
            parents,
            lamport,
            1,
            EventKind::Message,
            vec![7; 8192],
        ))
        .unwrap();
    }
}

#[tokio::test]
async fn current_group_membership_warm_queries_read_no_historical_bodies() {
    for count in [32, 128] {
        let root = tempfile::tempdir().unwrap();
        let (node, _) = super::tests::node(&root.path().join("alice"));
        let peer = crate::identity::device::DeviceIdentity::generate();
        let group = node
            .create_channel("Query", vec![peer.public()])
            .await
            .unwrap();
        append_messages(&node, group, count);
        crate::node::test_support::settle_group_metadata(&node, group);
        let before = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        for _ in 0..3 {
            assert!(node.group_member(&peer.public()).unwrap());
        }
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            before,
            "ready admission must not re-read {count} historical Message bodies"
        );
    }
}

#[tokio::test]
async fn current_group_membership_unapplied_leave_fails_closed_and_advances_boundedly() {
    let root = tempfile::tempdir().unwrap();
    let (node, _) = super::tests::node(&root.path().join("alice"));
    let peer = crate::identity::device::DeviceIdentity::generate();
    let group = node
        .create_channel("Query", vec![peer.public()])
        .await
        .unwrap();
    crate::node::test_support::settle_group_metadata(&node, group);
    append_messages(&node, group, 128);
    let leave = {
        let log = node.log.lock().unwrap();
        let (parents, lamport) = log.prepare(&group).unwrap();
        Event::new(
            &peer,
            group,
            1,
            parents,
            lamport,
            1,
            EventKind::ChannelLeave,
            vec![],
        )
    };
    node.log.lock().unwrap().append(leave).unwrap();
    assert!(
        node.channels
            .lock()
            .unwrap()
            .state(&group)
            .unwrap()
            .members()
            .contains(&peer.public()),
        "fixture must retain the stale live-book grant"
    );
    let mut completed = false;
    for _ in 0..1024 {
        let before = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        let result = node.group_member(&peer.public());
        let after = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        assert!(
            after.0.saturating_sub(before.0) <= 64,
            "one pending membership turn must retain its preparation body-read budget"
        );
        match result {
            Ok(false) => {
                completed = true;
                break;
            }
            Err(LogError::StaleCursor) => {}
            Ok(true) => {
                panic!("unapplied signed leave must not authorize the stale live-book member")
            }
            Err(error) => panic!("membership preparation failed: {error}"),
        }
    }
    assert!(
        completed,
        "bounded membership turns must eventually consume the signed leave"
    );
    let before = node
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .body_read_counts();
    assert!(!node.group_member(&peer.public()).unwrap());
    assert_eq!(
        node.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts(),
        before
    );
}

#[tokio::test]
async fn group_voice_member_requires_the_exact_accepted_device() {
    let root = tempfile::tempdir().unwrap();
    let (node, _) = super::tests::node(&root.path().join("alice"));
    let (invitee, _) = super::tests::node(&root.path().join("bob"));
    let group = node
        .create_channel_invited("Voice", vec![invitee.identity.public()])
        .await
        .unwrap();
    let accepted_group = node
        .create_channel("Accepted voice", vec![invitee.identity.public()])
        .await
        .unwrap();
    crate::node::test_support::settle_group_metadata(&node, accepted_group);
    node.roster.lock().unwrap().update(
        &invitee.own_group_proof(),
        std::net::Ipv4Addr::LOCALHOST.into(),
        &node.user_id(),
    );
    crate::node::test_support::settle_group_metadata(&node, group);
    assert_eq!(
        node.group_voice_member(group, &node.user_id()).unwrap(),
        Some(node.account_id())
    );
    assert_eq!(
        node.group_voice_member(group, &invitee.user_id()).unwrap(),
        None
    );
    assert_eq!(
        node.group_voice_member(accepted_group, &invitee.user_id())
            .unwrap(),
        Some(invitee.account_id())
    );
    assert_eq!(
        node.group_voice_member(
            crate::eventlog::ConversationId::new([91; 32]),
            &node.user_id()
        )
        .unwrap(),
        None
    );
}
