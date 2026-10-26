//! Search regressions live beside the search implementation so unrelated node
//! features can evolve without editing the large shared node test file.

use super::*;
use crate::discovery::announce::Announce;
use crate::discovery::roster::Roster;
use crate::identity::device::DeviceIdentity;
use crate::node::MessageProtection;
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

#[test]
fn canonical_projection_decodes_only_selected_rows_for_recent_and_exact_context() {
    use crate::node::DmEnvelope;
    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let own = node.account_id();
    let contact = "a".repeat(32);
    let conversation = account_conversation_id(&own, &contact);
    for index in 1..=128u64 {
        let mut id = [1; 32];
        id[..8].copy_from_slice(&index.to_be_bytes());
        let envelope = DmEnvelope::new(
            own.clone(),
            contact.clone(),
            id,
            MessageBody::new(vec![b'x'; 8192], None).encode(),
        );
        node.sentlog
            .lock()
            .unwrap()
            .record(conversation, index, index, &envelope.encode())
            .unwrap();
    }
    super::super::queries::reset_projection_decodes();
    let recent = node.account_history(&contact, 3);
    assert_eq!(recent.len(), 3);
    assert_eq!(super::super::queries::projection_decodes(), 3);
    let mut middle = [1; 32];
    middle[..8].copy_from_slice(&64u64.to_be_bytes());
    super::super::queries::reset_projection_decodes();
    let context = node
        .search_context(&SearchReference::Account {
            account_id: contact,
            message_id: Some(EventId::new(middle)),
        })
        .unwrap();
    assert_eq!(context.messages.len(), 51);
    assert_eq!(context.newer_entries, 64);
    assert_eq!(super::super::queries::projection_decodes(), 51);
}

#[test]
fn recent_search_global_limit_matches_full_order_across_conversations() {
    use crate::node::DmEnvelope;
    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let own = node.account_id();
    for (peer, times) in [("a".repeat(32), [9, 1, 5]), ("b".repeat(32), [7, 10, 5])] {
        for (index, time) in times.into_iter().enumerate() {
            let envelope = DmEnvelope::new(
                own.clone(),
                peer.clone(),
                [index as u8 + 1; 32],
                MessageBody::new(b"bounded token".to_vec(), None).encode(),
            );
            node.sentlog
                .lock()
                .unwrap()
                .record(
                    account_conversation_id(&own, &peer),
                    index as u64,
                    time,
                    &envelope.encode(),
                )
                .unwrap();
        }
    }
    let all = node.search_recent("token");
    let limited = node.search_recent_limited("token", 4);
    assert_eq!(
        limited
            .hits
            .iter()
            .map(|hit| (&hit.reference, hit.hit.wall_clock))
            .collect::<Vec<_>>(),
        all.hits[..4]
            .iter()
            .map(|hit| (&hit.reference, hit.hit.wall_clock))
            .collect::<Vec<_>>()
    );
    assert!(node.search_recent_limited("token", 0).hits.is_empty());
}

#[tokio::test]
async fn channel_projection_decodes_only_selected_rows_and_preserves_exact_context() {
    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let channel = node.create_channel("retained", vec![]).await.unwrap();
    let body = MessageBody::new(vec![b'x'; 8192], None).encode();
    for index in 1..=128u64 {
        let mut id = [1; 32];
        id[..8].copy_from_slice(&index.to_be_bytes());
        node.received
            .lock()
            .unwrap()
            .record(channel, "peer".to_string(), index, &body, EventId::new(id))
            .unwrap();
    }
    super::super::queries::reset_projection_decodes();
    let recent = node.channel_history(channel, 3);
    assert_eq!(recent.len(), 3);
    assert_eq!(super::super::queries::projection_decodes(), 3);
    let mut middle = [1; 32];
    middle[..8].copy_from_slice(&64u64.to_be_bytes());
    super::super::queries::reset_projection_decodes();
    let context = node
        .search_context(&SearchReference::Channel {
            channel_id: channel,
            message_id: Some(EventId::new(middle)),
        })
        .unwrap();
    assert_eq!(context.messages.len(), 51);
    assert_eq!(context.newer_entries, 64);
    assert_eq!(super::super::queries::projection_decodes(), 51);
}

#[test]
fn borrowed_account_envelope_validation_matches_owned_decoder() {
    let envelope = crate::node::DmEnvelope::new(
        "sender".to_string(),
        "recipient".to_string(),
        [9; 32],
        vec![0, 255, 127, 42],
    );
    let encoded = envelope.encode();
    let mut variants = (0..=encoded.len())
        .map(|len| encoded[..len].to_vec())
        .collect::<Vec<_>>();
    let mut trailing = encoded.clone();
    trailing.push(0);
    variants.push(trailing);
    for index in 0..encoded.len() {
        let mut changed = encoded.clone();
        changed[index] ^= 0xff;
        variants.push(changed);
    }
    variants.push(vec![0; 256 * 1024 + 1]);
    for wire in variants {
        let owned = crate::node::DmEnvelope::decode(&wire);
        let borrowed = crate::node::DmEnvelope::decode_ref(&wire);
        assert_eq!(owned.is_some(), borrowed.is_some());
        if let (Some(owned), Some(borrowed)) = (owned, borrowed) {
            assert_eq!(owned.msg_id, borrowed.msg_id);
            assert_eq!(owned.body, borrowed.body);
        }
    }
}

#[test]
fn device_history_keeps_envelope_deadline_metadata_for_legacy_stored_rows() {
    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let peer = DeviceIdentity::generate();
    let conversation = dm_conversation_id(&node.identity.public(), &peer.public());
    let deadline = crate::node::node::now_millis() + 60_000;
    let envelope = crate::node::DmEnvelope::new(
        "sender".into(),
        "recipient".into(),
        [9; 32],
        crate::expiry::wrap(
            &MessageBody::new(b"retained".to_vec(), None).encode(),
            Some(deadline),
        ),
    );
    node.received
        .lock()
        .unwrap()
        .record(
            conversation,
            peer.user_id(),
            1,
            &envelope.encode(),
            EventId::new([9; 32]),
        )
        .unwrap();
    let history = node.dm_history(&peer.public(), 1);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].expires_at_ms, Some(deadline));
}

#[test]
fn bounded_history_with_authenticated_peer_recall_never_clones_retained_plaintext() {
    use crate::eventlog::event::{Event, EventKind};
    use crate::node::dm_envelope::{DmEnvelope, RecallEnvelope};
    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let peer = DeviceIdentity::generate();
    let peer_account = crate::identity::account::Account::generate();
    let account_id = peer_account.account_id();
    node.roster.lock().unwrap().update(
        &Announce::new_with_account(&peer, &peer_account, "Peer", 1),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &node.user_id(),
    );
    let own = node.account_id();
    let conversation = account_conversation_id(&own, &account_id);
    for index in 1..=128u64 {
        for incoming in [false, true] {
            let mut id = [if incoming { 2 } else { 1 }; 32];
            id[..8].copy_from_slice(&index.to_be_bytes());
            let envelope = DmEnvelope::new(
                if incoming {
                    account_id.clone()
                } else {
                    own.clone()
                },
                if incoming {
                    own.clone()
                } else {
                    account_id.clone()
                },
                id,
                MessageBody::new(vec![b'x'; 8192], None).encode(),
            )
            .encode();
            if incoming {
                node.received
                    .lock()
                    .unwrap()
                    .record(
                        conversation,
                        account_id.clone(),
                        index,
                        &envelope,
                        EventId::new(id),
                    )
                    .unwrap();
            } else {
                node.sentlog
                    .lock()
                    .unwrap()
                    .record(conversation, index, index, &envelope)
                    .unwrap();
            }
        }
    }
    let mut target = [2; 32];
    target[..8].copy_from_slice(&64u64.to_be_bytes());
    let claim = RecallEnvelope::new(account_id.clone(), own, target).encode();
    let sealed = crate::dm::seal(&peer, &node.identity.public().x25519_pub, &claim).unwrap();
    let delete = Event::new(
        &peer,
        dm_conversation_id(&node.identity.public(), &peer.public()),
        1,
        vec![],
        1,
        129,
        EventKind::Delete,
        sealed,
    );
    node.log.lock().unwrap().append(delete).unwrap();
    super::super::received_log::reset_history_clone_bytes();
    let history = node.account_history(&account_id, 3);
    assert_eq!(history.len(), 3);
    assert_eq!(
        super::super::received_log::history_clone_bytes(),
        0,
        "peer recall author verification must not clone retained sent/received bodies"
    );
    assert!(
        node.recalled_targets_account(&account_id)
            .contains(&EventId::new(target))
    );
    let mut neighbor = [2; 32];
    neighbor[..8].copy_from_slice(&65u64.to_be_bytes());
    super::super::received_log::reset_history_clone_bytes();
    let context = node
        .search_context(&SearchReference::Account {
            account_id,
            message_id: Some(EventId::new(neighbor)),
        })
        .unwrap();
    assert_eq!(context.messages.len(), 51);
    assert!(
        context
            .messages
            .iter()
            .any(|entry| entry.id == EventId::new(target)
                && entry.recalled
                && entry.text.is_empty())
    );
    assert_eq!(super::super::received_log::history_clone_bytes(), 0);
}

#[test]
fn canonical_dedup_keeps_earliest_clock_copy_despite_reverse_insertion() {
    use crate::node::DmEnvelope;
    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let own = node.account_id();
    let contact = "a".repeat(32);
    let conversation = account_conversation_id(&own, &contact);
    for (clock, text) in [
        (100, b"later copy".as_slice()),
        (1, b"first canonical copy".as_slice()),
    ] {
        let envelope = DmEnvelope::new(
            contact.clone(),
            own.clone(),
            [7; 32],
            MessageBody::new(text.to_vec(), None).encode(),
        );
        node.received
            .lock()
            .unwrap()
            .record(
                conversation,
                contact.clone(),
                clock,
                &envelope.encode(),
                EventId::new([clock as u8; 32]),
            )
            .unwrap();
    }
    let history = node.account_history(&contact, 1);
    assert_eq!(history[0].wall_clock, 1);
    assert_eq!(history[0].text, b"first canonical copy");
}

#[test]
fn malformed_nested_protection_is_absent_from_history_search_and_context() {
    use crate::node::DmEnvelope;

    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let own = node.account_id();
    let contact = "a".repeat(32);
    let id = EventId::new([7; 32]);
    let protected =
        MessageBody::protected(b"secret".to_vec(), None, MessageProtection::Click).encode();
    let mut malformed = b"MTEX1".to_vec();
    malformed.extend_from_slice(&u64::MAX.to_le_bytes());
    malformed.extend_from_slice(&protected);
    let envelope = DmEnvelope::new(contact.clone(), own.clone(), [7; 32], malformed);
    node.received
        .lock()
        .unwrap()
        .record(
            account_conversation_id(&own, &contact),
            contact.clone(),
            1,
            &envelope.encode(),
            id,
        )
        .unwrap();

    let history = node.account_history(&contact, 10);
    assert_eq!(history.len(), 1);
    assert!(history[0].text.is_empty());
    for query in ["MTPR1", "MTEX1", "c2VjcmV0", "secret"] {
        assert!(node.search(query).is_empty());
        assert!(node.search_recent(query).hits.is_empty());
    }
    let reference = SearchReference::Account {
        account_id: contact,
        message_id: Some(id),
    };
    let context = node.search_context(&reference).unwrap();
    assert_eq!(context.messages.len(), 1);
    assert!(context.messages[0].text.is_empty());
    assert!(node.protected_message_content(&reference).is_none());
}

#[tokio::test]
async fn protected_channel_exact_reveal_excludes_search_context_recall_and_wrong_namespace() {
    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let channel = node.create_channel("protected", vec![]).await.unwrap();
    let other = node.create_channel("other", vec![]).await.unwrap();
    let id = node
        .enqueue_protected_channel_message_reply_expiring_if(
            channel,
            b"private token",
            None,
            None,
            MessageProtection::Click,
            |accept| accept(),
        )
        .unwrap();
    let reference = SearchReference::Channel {
        channel_id: channel,
        message_id: Some(id),
    };
    let history = node.channel_history(channel, 10);
    assert_eq!(history[0].protection, Some(MessageProtection::Click));
    assert!(history[0].text.is_empty());
    assert!(node.search("private").is_empty());
    assert!(node.search_recent("private").hits.is_empty());
    assert!(
        node.search_context(&reference).unwrap().messages[0]
            .text
            .is_empty()
    );
    assert_eq!(
        node.protected_message_content(&reference),
        Some((MessageProtection::Click, b"private token".to_vec()))
    );
    assert!(
        node.protected_message_content(&SearchReference::Channel {
            channel_id: other,
            message_id: Some(id)
        })
        .is_none()
    );
    assert!(
        node.protected_message_content(&SearchReference::Channel {
            channel_id: channel,
            message_id: None
        })
        .is_none()
    );
    node.recall_channel(channel, id).await.unwrap();
    assert!(node.protected_message_content(&reference).is_none());
    assert!(node.channel_history(channel, 10)[0].recalled_text.is_none());
}

#[tokio::test]
async fn protected_channel_retains_redaction_and_exact_content_after_restart_and_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let secret = node.identity.secret_bytes();
    let channel = node
        .create_channel("persistent protected", vec![])
        .await
        .unwrap();
    let id = node
        .enqueue_protected_channel_message_reply_expiring_if(
            channel,
            b"retained secret",
            None,
            None,
            MessageProtection::Authenticate,
            |accept| accept(),
        )
        .unwrap();
    let reference = SearchReference::Channel {
        channel_id: channel,
        message_id: Some(id),
    };
    let mut snapshot = Vec::new();
    node.write_recovery_snapshot(&mut snapshot, 100).unwrap();
    let mut retained = None;
    crate::recovery::snapshot::read_snapshot(std::io::Cursor::new(snapshot), |record| {
        if let crate::recovery::snapshot::SnapshotRecordV1::SentDm {
            message_id,
            plaintext,
            ..
        } = record
            && message_id == *id.as_bytes()
        {
            assert!(MessageBody::decode(&plaintext).text.is_empty());
            assert_eq!(
                MessageBody::decode_revealed(&plaintext).text,
                b"retained secret"
            );
            retained = Some(plaintext);
        }
        Ok(())
    })
    .unwrap();
    assert!(retained.is_some());
    drop(node);
    let (dm, _) = mpsc::unbounded_channel();
    let (ch, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    let reopened = Node::open(
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        ch,
        file,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let history = reopened.channel_history(channel, 10);
    assert_eq!(history[0].id, id);
    assert_eq!(history[0].protection, Some(MessageProtection::Authenticate));
    assert!(history[0].text.is_empty());
    assert_eq!(
        reopened.protected_message_content(&reference),
        Some((MessageProtection::Authenticate, b"retained secret".to_vec()))
    );
}

#[tokio::test]
async fn protected_images_require_exact_reveal_and_block_ordinary_bytes_and_exports() {
    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let image = dir.path().join("sensitive-name.png");
    let bytes = b"\x89PNG\r\n\x1a\nprotected image pixels";
    std::fs::write(&image, bytes).unwrap();
    let channel = node
        .create_channel("protected image", vec![])
        .await
        .unwrap();
    let (id, file) = node
        .enqueue_protected_file_channel_progress_expiring_if(
            channel,
            &image,
            crate::file::FileKind::Media,
            |_| {},
            None,
            MessageProtection::Authenticate,
            |accept| accept(),
        )
        .await
        .unwrap();
    let reference = SearchReference::Channel {
        channel_id: channel,
        message_id: Some(id),
    };
    let entry = &node.channel_history(channel, 10)[0];
    assert_eq!(entry.protection, Some(MessageProtection::Authenticate));
    assert!(!entry.file.as_ref().unwrap().name.contains("sensitive"));
    {
        let book = node.files.lock().unwrap();
        let manifest = book.manifest(&file).unwrap();
        assert!(!book.accepts(manifest.unprotected()));
        let downgraded = crate::file::AnyManifest::Protected {
            protection: MessageProtection::Click,
            inner: Box::new(manifest.unprotected().clone()),
        };
        assert!(!book.accepts(&downgraded));
        assert!(!crate::node::files::same_file_transfer(
            manifest,
            manifest.unprotected()
        ));
    }
    {
        let rows = node.received_files.lock().unwrap();
        let wire = &rows.entries_ref(&channel)[0].plaintext;
        assert!(crate::file::FileManifestV3::decode(wire).is_none());
        assert!(crate::file::FileManifest::decode(wire).is_none());
        for length in 0..wire.len() {
            assert!(crate::node::files::validated_manifest(&wire[..length]).is_none());
        }
        let mut invalid = wire.clone();
        invalid[3] = b'9';
        assert!(crate::node::files::validated_manifest(&invalid).is_none());
        let mut invalid = wire.clone();
        invalid[4] = 255;
        assert!(crate::node::files::validated_manifest(&invalid).is_none());
    }
    assert!(node.read_media(file).is_none());
    assert!(node.read_file(file).is_err());
    assert!(!node.file_ready_to_save(file));
    let output = dir.path().join("export.png");
    assert!(node.save_file(file, &output).is_err());
    assert!(!output.exists());
    assert!(node.save_file_into_dir(file, dir.path()).is_err());
    assert_eq!(
        node.protected_image_protection(&reference),
        Some(MessageProtection::Authenticate)
    );
    assert_eq!(
        node.protected_image_content(&reference),
        Some((MessageProtection::Authenticate, bytes.to_vec()))
    );
    assert!(node.protected_message_content(&reference).is_none());
    let wrong = node.create_channel("wrong", vec![]).await.unwrap();
    assert!(
        node.protected_image_content(&SearchReference::Channel {
            channel_id: wrong,
            message_id: Some(id)
        })
        .is_none()
    );
    assert!(node.recall_account(&"a".repeat(32), id).await.is_err());
    let (ordinary_id, _) = node
        .enqueue_file_channel_progress_expiring_if(
            channel,
            &image,
            crate::file::FileKind::File,
            |_| {},
            None,
            |accept| accept(),
        )
        .await
        .unwrap();
    assert!(node.recall_channel(channel, ordinary_id).await.is_err());
    let foreign_author = DeviceIdentity::generate();
    let foreign = crate::eventlog::Event::new(
        &foreign_author,
        channel,
        1,
        vec![],
        1,
        crate::node::node::now_millis(),
        crate::eventlog::EventKind::FileManifest,
        vec![],
    );
    node.log.lock().unwrap().append(foreign.clone()).unwrap();
    assert!(node.recall_channel(channel, foreign.id).await.is_err());
    node.recall_channel(channel, id).await.unwrap();
    assert!(node.protected_image_content(&reference).is_none());
    let (account_id, account_file) = node
        .enqueue_protected_file_to_account_progress_expiring_if(
            &node.account_id(),
            &image,
            crate::file::FileKind::Media,
            |_| {},
            None,
            MessageProtection::Click,
            |accept| accept(),
        )
        .await
        .unwrap();
    let account_reference = SearchReference::Account {
        account_id: node.account_id(),
        message_id: Some(account_id),
    };
    assert!(node.read_media(account_file).is_none());
    assert_eq!(
        node.protected_image_content(&account_reference),
        Some((MessageProtection::Click, bytes.to_vec()))
    );
    let video = dir.path().join("video.mp4");
    std::fs::write(&video, b"video").unwrap();
    assert!(
        node.enqueue_protected_file_channel_progress_expiring_if(
            channel,
            &video,
            crate::file::FileKind::Media,
            |_| {},
            None,
            MessageProtection::Click,
            |accept| accept()
        )
        .await
        .is_err()
    );
    assert!(
        node.enqueue_protected_file_to_account_progress_expiring_if(
            &node.account_id(),
            &image,
            crate::file::FileKind::File,
            |_| {},
            None,
            MessageProtection::Click,
            |accept| accept()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn protected_image_without_manifest_never_uses_ordinary_media_read() {
    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let path = dir.path().join("private.png");
    let bytes = b"\x89PNG\r\n\x1a\nprivate image";
    std::fs::write(&path, bytes).unwrap();
    let (_, file) = node
        .enqueue_protected_file_to_account_progress_expiring_if(
            &node.account_id(),
            &path,
            crate::file::FileKind::Media,
            |_| {},
            None,
            MessageProtection::Authenticate,
            |accept| accept(),
        )
        .await
        .unwrap();
    // Lifecycle cleanup retires the manifest before deleting the encrypted blob.
    // Model that intermediate state without relying on scheduler timing.
    *node.files.lock().unwrap() = crate::node::filebook::FileBook::new();
    assert_eq!(node.media.read(file), Some(bytes.to_vec()));
    assert!(node.read_media(file).is_none());
}

#[tokio::test]
async fn protected_image_retains_policy_and_exact_bytes_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let node = recent_search_node(dir.path());
    let secret = node.identity.secret_bytes();
    let image = dir.path().join("original.png");
    let bytes = b"\x89PNG\r\n\x1a\npersistent pixels";
    std::fs::write(&image, bytes).unwrap();
    let account = node.account_id();
    let (id, file) = node
        .enqueue_protected_file_to_account_progress_expiring_if(
            &account,
            &image,
            crate::file::FileKind::Media,
            |_| {},
            None,
            MessageProtection::Click,
            |accept| accept(),
        )
        .await
        .unwrap();
    let reference = SearchReference::Account {
        account_id: account.clone(),
        message_id: Some(id),
    };
    drop(node);
    let (dm, _) = mpsc::unbounded_channel();
    let (ch, _) = mpsc::unbounded_channel();
    let (files, _) = mpsc::unbounded_channel();
    let node = Node::open(
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        ch,
        files,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    assert_eq!(
        node.account_history(&account, 10)[0].protection,
        Some(MessageProtection::Click)
    );
    assert!(node.read_media(file).is_none());
    assert!(node.read_file(file).is_err());
    assert_eq!(
        node.protected_image_content(&reference),
        Some((MessageProtection::Click, bytes.to_vec()))
    );
    node.recall_account(&account, id).await.unwrap();
    assert!(node.protected_image_content(&reference).is_none());
}

#[tokio::test]
async fn protected_image_expiry_hides_explicit_and_ordinary_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = recent_search_node(dir.path());
    let image = dir.path().join("image.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\nimage").unwrap();
    let deadline = crate::node::node::now_millis() + 300_000;
    let (id, file) = node
        .enqueue_protected_file_to_account_progress_expiring_if(
            &node.account_id(),
            &image,
            crate::file::FileKind::Media,
            |_| {},
            Some(deadline),
            MessageProtection::Authenticate,
            |accept| accept(),
        )
        .await
        .unwrap();
    let reference = SearchReference::Account {
        account_id: node.account_id(),
        message_id: Some(id),
    };
    assert!(node.protected_image_content(&reference).is_some());
    Arc::get_mut(&mut node).unwrap().expiry_clock_start = (std::time::Instant::now(), deadline + 1);
    assert!(node.protected_image_content(&reference).is_none());
    assert!(node.protected_image_protection(&reference).is_none());
    assert!(node.read_media(file).is_none());
    assert!(node.read_file(file).is_err());
}
