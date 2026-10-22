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
    assert_eq!(node.search_recent("NOON").hits.len(), 1);
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
    assert!(node.search_recent("NOON").hits.is_empty());
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
    assert_eq!(node.search_recent("PROJECT").hits.len(), 1);
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
    assert!(node.search_recent("diagnostic").hits.is_empty());

    // A durable proof may still describe the same device as an ordinary peer
    // while its current signed announcement advertises the relay role.
    node.configure_privacy(
        dir.path(),
        "pw",
        &node.signed_announce("Me", 1),
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    node.privacy
        .state
        .write()
        .unwrap()
        .as_mut()
        .unwrap()
        .proofs
        .record(&Announce::new_with_account(
            &relay,
            &relay_account,
            "Former contact",
            1,
        ))
        .unwrap();
    assert!(node.search("diagnostic").is_empty());
    assert!(node.search_recent("diagnostic").hits.is_empty());
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
    assert!(node.search_recent("withdrawn").hits.is_empty());
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
    assert!(node.search_recent("withdrawn").hits.is_empty());
    assert_eq!(node.search("retained").len(), 1);
}

fn recent_search_node(dir: &std::path::Path) -> Arc<Node> {
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        file,
        &dir.join("messages.log"),
        &dir.join("sent.log"),
        "pw",
    )
    .unwrap()
}

#[test]
fn recent_search_distinguishes_equal_account_text_and_deduplicates_sibling_copies() {
    use crate::eventlog::event::EventId;
    use crate::node::DmEnvelope;

    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let own = node.account_id();
    let contact = "a".repeat(32);
    let conversation = account_conversation_id(&own, &contact);
    let body = MessageBody::new(b"identical project notes".to_vec(), None).encode();
    for index in 1..=2 {
        let envelope = DmEnvelope::new(own.clone(), contact.clone(), [index; 32], body.clone());
        node.sentlog
            .lock()
            .unwrap()
            .record(conversation, u64::from(index), 42, &envelope.encode())
            .unwrap();
        // A sibling copy has a transport ID distinct from the logical ID.
        node.received
            .lock()
            .unwrap()
            .record(
                conversation,
                own.clone(),
                42,
                &envelope.encode(),
                EventId::new([index + 10; 32]),
            )
            .unwrap();
    }
    let response = node.search_recent("PROJECT");
    assert_eq!(response.per_conversation_limit, 2000);
    assert_eq!(response.hits.len(), 2);
    let history = node.account_history(&contact, 10);
    for (hit, expected) in response.hits.iter().zip(history.iter().rev()) {
        assert_eq!(
            hit.reference,
            SearchReference::Account {
                account_id: contact.clone(),
                message_id: Some(expected.id)
            }
        );
        assert!(hit.hit.from_me);
        assert_eq!(hit.hit.target, contact);
        assert_eq!(hit.hit.text, b"identical project notes");
    }
}

#[tokio::test]
async fn recent_search_channel_reference_matches_history_and_excludes_recall() {
    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let channel = node.create_channel("project", vec![]).await.unwrap();
    node.send_channel_message(channel, b"canonical channel token")
        .await
        .unwrap();
    let id = node.channel_history(channel, 10)[0].id;
    let response = node.search_recent("channel token");
    assert_eq!(response.hits.len(), 1);
    assert_eq!(
        response.hits[0].reference,
        SearchReference::Channel {
            channel_id: channel,
            message_id: Some(id)
        }
    );
    node.recall_channel(channel, id).await.unwrap();
    assert!(node.search_recent("channel token").hits.is_empty());
}

#[tokio::test]
async fn recent_search_recovered_channel_send_preserves_original_id() {
    use crate::eventlog::event::EventId;
    use crate::node::recovered_sent_ids::RecoveredSentIds;

    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let channel = node.create_channel("archive", vec![]).await.unwrap();
    let plaintext = MessageBody::new(b"recovered archive token".to_vec(), None).encode();
    node.sentlog
        .lock()
        .unwrap()
        .record(channel, 900, 42, &plaintext)
        .unwrap();
    let entry = node.sentlog.lock().unwrap().entries(&channel)[0].clone();
    let original = EventId::new([99; 32]);
    let mut index = RecoveredSentIds::open(&dir.path().join("recovered.log"), "pw").unwrap();
    index.record(&entry, original).unwrap();
    *node.recovered_sent_ids.lock().unwrap() = Some(index);
    assert_eq!(node.channel_history(channel, 10)[0].id, original);
    assert_eq!(
        node.search_recent("archive token").hits[0].reference,
        SearchReference::Channel {
            channel_id: channel,
            message_id: Some(original)
        }
    );
}

#[test]
fn recent_search_legacy_receive_is_device_scoped_and_unresolved_send_is_unavailable() {
    use crate::eventlog::event::EventId;

    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let peer = DeviceIdentity::generate();
    node.roster.lock().unwrap().update(
        &Announce::new_with_account(
            &peer,
            &crate::identity::account::Account::generate(),
            "Legacy",
            1,
        ),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &node.user_id(),
    );
    let conversation = dm_conversation_id(&node.identity.public(), &peer.public());
    let body = MessageBody::new(b"legacy token".to_vec(), None).encode();
    let received_id = EventId::new([17; 32]);
    node.received
        .lock()
        .unwrap()
        .record(conversation, peer.user_id(), 10, &body, received_id)
        .unwrap();
    node.sentlog
        .lock()
        .unwrap()
        .record(conversation, 999, 20, &body)
        .unwrap();
    let response = node.search_recent("legacy token");
    assert_eq!(response.hits.len(), 2);
    assert_eq!(
        response.hits[0].reference,
        SearchReference::LegacyDevice {
            device_id: peer.user_id(),
            message_id: None
        }
    );
    assert_eq!(
        response.hits[1].reference,
        SearchReference::LegacyDevice {
            device_id: peer.user_id(),
            message_id: Some(received_id)
        }
    );
}

#[test]
fn recent_search_reports_recent_window_and_does_not_find_older_match() {
    use crate::node::DmEnvelope;

    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let own = node.account_id();
    let contact = "b".repeat(32);
    let conversation = account_conversation_id(&own, &contact);
    for index in 0..=2000u64 {
        let mut id = [1; 32];
        id[..8].copy_from_slice(&index.to_le_bytes());
        let text = if index == 0 {
            b"older needle".as_slice()
        } else {
            b"recent row".as_slice()
        };
        let envelope = DmEnvelope::new(
            own.clone(),
            contact.clone(),
            id,
            MessageBody::new(text.to_vec(), None).encode(),
        );
        node.sentlog
            .lock()
            .unwrap()
            .record(conversation, index, index, &envelope.encode())
            .unwrap();
    }
    let response = node.search_recent("needle");
    assert_eq!(response.per_conversation_limit, 2000);
    assert!(response.hits.is_empty());
    assert_eq!(node.search_recent("recent row").hits.len(), 2000);
    assert!(node.search_recent("  ").hits.is_empty());
}

#[test]
fn recent_search_excludes_expired_account_plaintext() {
    use crate::node::DmEnvelope;

    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let own = node.account_id();
    let contact = "c".repeat(32);
    let conversation = account_conversation_id(&own, &contact);
    let envelope = DmEnvelope::new(
        own,
        contact,
        [11; 32],
        crate::expiry::wrap(
            &MessageBody::new(b"expired token".to_vec(), None).encode(),
            Some(1),
        ),
    );
    node.sentlog
        .lock()
        .unwrap()
        .record(conversation, 1, 1, &envelope.encode())
        .unwrap();
    assert!(node.search_recent("expired token").hits.is_empty());
}

#[tokio::test]
async fn recent_search_includes_durably_accepted_message_before_sidecar_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let peer = DeviceIdentity::generate();
    let account = crate::identity::account::Account::generate();
    node.roster.lock().unwrap().update(
        &Announce::new_with_account(&peer, &account, "Peer", 1),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &node.user_id(),
    );
    node.sentlog
        .lock()
        .unwrap()
        .fail_appends_for_test(true)
        .unwrap();
    let id = node
        .enqueue_to_account(&account.account_id(), b"accepted WAL token", None)
        .await
        .unwrap();
    assert_eq!(
        node.delivery.lock().unwrap().pending_transactions().len(),
        1
    );
    assert_eq!(node.account_history(&account.account_id(), 10)[0].id, id);
    let response = node.search_recent("WAL token");
    assert_eq!(response.hits.len(), 1);
    assert_eq!(
        response.hits[0].reference,
        SearchReference::Account {
            account_id: account.account_id(),
            message_id: Some(id)
        }
    );
    node.sentlog
        .lock()
        .unwrap()
        .fail_appends_for_test(false)
        .unwrap();
}

#[test]
fn search_context_selects_exact_old_account_id_and_bounded_canonical_neighbors() {
    use crate::{eventlog::event::EventId, node::DmEnvelope};
    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let own = node.account_id();
    let contact = "d".repeat(32);
    let conversation = account_conversation_id(&own, &contact);
    for index in 1..=600u64 {
        let mut id = [1; 32];
        id[..8].copy_from_slice(&index.to_le_bytes());
        let envelope = DmEnvelope::new(
            own.clone(),
            contact.clone(),
            id,
            MessageBody::new(b"equal text".to_vec(), None).encode(),
        );
        node.sentlog
            .lock()
            .unwrap()
            .record(conversation, index, index, &envelope.encode())
            .unwrap();
    }
    let history = node.account_history(&contact, usize::MAX);
    let reference = SearchReference::Account {
        account_id: contact.clone(),
        message_id: Some(history[50].id),
    };
    let context = node.search_context(&reference).unwrap();
    assert_eq!(context.newer_entries, 549);
    assert_eq!(context.messages.len(), 51);
    assert_eq!(
        context.messages.iter().map(|e| e.id).collect::<Vec<_>>(),
        history[25..76].iter().map(|e| e.id).collect::<Vec<_>>()
    );
    assert!(context.messages.iter().all(|e| e.recalled_text.is_none()));
    for message_id in [
        None,
        Some(EventId::new([0; 32])),
        Some(EventId::new([99; 32])),
    ] {
        assert!(
            node.search_context(&SearchReference::Account {
                account_id: contact.clone(),
                message_id
            })
            .is_none()
        );
    }
    assert!(
        node.search_context(&SearchReference::Account {
            account_id: "e".repeat(32),
            message_id: Some(history[50].id)
        })
        .is_none()
    );
}

#[tokio::test]
async fn search_context_rejects_recall_and_expiry_and_scrubs_neighbor_recalled_text() {
    use crate::{eventlog::event::EventId, node::DmEnvelope};
    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let own = node.account_id();
    let contact = "c".repeat(32);
    let conversation = account_conversation_id(&own, &contact);
    for index in 1..=3u8 {
        let body = MessageBody::new(b"secret".to_vec(), None).encode();
        let body = if index == 3 {
            crate::expiry::wrap(&body, Some(1))
        } else {
            body
        };
        let envelope = DmEnvelope::new(own.clone(), contact.clone(), [index; 32], body);
        node.sentlog
            .lock()
            .unwrap()
            .record(
                conversation,
                u64::from(index),
                crate::node::node::now_millis(),
                &envelope.encode(),
            )
            .unwrap();
    }
    node.recall_account(&contact, EventId::new([1; 32]))
        .await
        .unwrap();
    for index in [1, 3] {
        assert!(
            node.search_context(&SearchReference::Account {
                account_id: contact.clone(),
                message_id: Some(EventId::new([index; 32]))
            })
            .is_none()
        );
    }
    let context = node
        .search_context(&SearchReference::Account {
            account_id: contact,
            message_id: Some(EventId::new([2; 32])),
        })
        .unwrap();
    assert!(context.messages.iter().any(|e| e.recalled));
    assert!(context.messages.iter().all(|e| e.recalled_text.is_none()));
}

#[test]
fn search_context_preserves_legacy_namespace_after_verified_account_binding() {
    use crate::eventlog::event::EventId;
    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let peer = DeviceIdentity::generate();
    let account = crate::identity::account::Account::generate();
    node.roster.lock().unwrap().update(
        &Announce::new_with_account(&peer, &account, "Legacy", 1),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &node.user_id(),
    );
    let conversation = dm_conversation_id(&node.identity.public(), &peer.public());
    let id = EventId::new([17; 32]);
    node.received
        .lock()
        .unwrap()
        .record(
            conversation,
            peer.user_id(),
            10,
            &MessageBody::new(b"legacy".to_vec(), None).encode(),
            id,
        )
        .unwrap();
    assert_eq!(
        node.search_context(&SearchReference::LegacyDevice {
            device_id: peer.user_id(),
            message_id: Some(id)
        })
        .unwrap()
        .messages[0]
            .text,
        b"legacy"
    );
    assert!(
        node.search_context(&SearchReference::Account {
            account_id: account.account_id(),
            message_id: Some(id)
        })
        .is_none()
    );
    node.roster.lock().unwrap().update(
        &Announce::new_post_office_with_account(&peer, &account, "Relay", 1),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &node.user_id(),
    );
    assert!(
        node.search_context(&SearchReference::LegacyDevice {
            device_id: peer.user_id(),
            message_id: Some(id)
        })
        .is_none()
    );
}

#[tokio::test]
async fn search_context_channel_uses_canonical_exact_identity() {
    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let channel = node.create_channel("context", vec![]).await.unwrap();
    for _ in 0..3 {
        node.send_channel_message(channel, b"duplicate")
            .await
            .unwrap();
    }
    let history = node.channel_history(channel, 10);
    let reference = SearchReference::Channel {
        channel_id: channel,
        message_id: Some(history[1].id),
    };
    assert_eq!(
        node.search_context(&reference)
            .unwrap()
            .messages
            .iter()
            .map(|e| e.id)
            .collect::<Vec<_>>(),
        history.iter().map(|e| e.id).collect::<Vec<_>>()
    );
    node.recall_channel(channel, history[1].id).await.unwrap();
    assert!(node.search_context(&reference).is_none());
}

#[test]
fn recent_search_preserves_expiry_deadline_from_canonical_history() {
    use crate::node::DmEnvelope;
    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let own = node.account_id();
    let contact = "b".repeat(32);
    let deadline = crate::node::node::now_millis() + 60_000;
    let envelope = DmEnvelope::new(
        own.clone(),
        contact.clone(),
        [42; 32],
        crate::expiry::wrap(
            &MessageBody::new(b"expiring token".to_vec(), None).encode(),
            Some(deadline),
        ),
    );
    node.sentlog
        .lock()
        .unwrap()
        .record(
            account_conversation_id(&own, &contact),
            1,
            crate::node::node::now_millis(),
            &envelope.encode(),
        )
        .unwrap();
    assert_eq!(
        node.search_recent("expiring token").hits[0].expires_at_ms,
        Some(deadline)
    );
}
