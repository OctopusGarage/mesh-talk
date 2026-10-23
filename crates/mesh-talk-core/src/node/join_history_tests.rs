use super::*;
use crate::discovery::Roster;
use crate::identity::device::DeviceIdentity;
use std::sync::{Arc, Mutex};

fn node(dir: &std::path::Path) -> Arc<Node> {
    let (dm, _) = tokio::sync::mpsc::unbounded_channel();
    let (channel, _) = tokio::sync::mpsc::unbounded_channel();
    let (file, _) = tokio::sync::mpsc::unbounded_channel();
    Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        file,
        &dir.join("events"),
        &dir.join("sent"),
        "pw",
    )
    .unwrap()
}

fn copy(from: &Node, to: &Node, channel: ConversationId) {
    let events = from
        .log
        .lock()
        .unwrap()
        .events(&channel)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    for event in events {
        to.log.lock().unwrap().append(event).unwrap();
    }
    to.process_channel(channel);
}

#[tokio::test]
async fn join_history_default_is_bounded_and_requires_acceptance() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let alice = node(a.path());
    let bob = node(b.path());
    let channel = alice
        .create_channel_invited("history", vec![])
        .await
        .unwrap();
    assert_eq!(
        alice.channel_history_policy(channel),
        HistoryPolicy::LastTenWithinThreeDays
    );
    for i in 0..14 {
        alice
            .send_channel_message(channel, format!("message {i}").as_bytes())
            .await
            .unwrap();
    }
    alice
        .add_channel_member(channel, bob.identity.public())
        .await
        .unwrap();
    copy(&alice, &bob, channel);
    assert!(
        bob.channel_history(channel, 100).is_empty(),
        "pending invite must not decrypt a history offer"
    );
    bob.accept_channel_invite(channel).await.unwrap();
    copy(&bob, &alice, channel);
    alice.retry_join_history().unwrap();
    copy(&alice, &bob, channel);
    let history = bob.channel_history(channel, 100);
    assert_eq!(history.len(), 10);
    assert_eq!(history[0].text, b"message 4");
    assert_eq!(history[9].text, b"message 13");
    copy(&alice, &bob, channel);
    assert_eq!(
        bob.channel_history(channel, 100).len(),
        10,
        "delivery replay must deduplicate"
    );
}

#[tokio::test]
async fn join_history_policy_is_owner_only_and_old_groups_default_off() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let alice = node(a.path());
    let bob = node(b.path());
    let channel = alice
        .create_channel("legacy", vec![bob.identity.public()])
        .await
        .unwrap();
    assert_eq!(alice.channel_history_policy(channel), HistoryPolicy::Off);
    copy(&alice, &bob, channel);
    assert!(
        bob.set_channel_history_policy(channel, HistoryPolicy::LastTen)
            .await
            .is_err()
    );
    alice
        .set_channel_history_policy(channel, HistoryPolicy::LastTen)
        .await
        .unwrap();
    copy(&alice, &bob, channel);
    assert_eq!(bob.channel_history_policy(channel), HistoryPolicy::LastTen);
}

#[test]
fn join_history_time_and_count_bounds() {
    let now = 10 * DAY_MS;
    let entries = (0..15)
        .map(|i| HistoryEntry {
            id: EventId::new([i; 32]),
            from_me: false,
            who: "author".into(),
            text: vec![i],
            protection: None,
            wall_clock: if i < 3 { now - 4 * DAY_MS } else { now - 1 },
            reply_to: None,
            file: None,
            recalled: false,
            recalled_text: None,
            sticker: None,
            expires_at_ms: None,
        })
        .collect::<Vec<_>>();
    let selected = select_history(entries.clone(), HistoryPolicy::LastTenWithinThreeDays, now);
    assert_eq!(selected.len(), 10);
    assert_eq!(selected[0].text, vec![5]);
    assert_eq!(
        select_history(entries.clone(), HistoryPolicy::LastThreeDays, now).len(),
        12
    );
    assert!(select_history(entries, HistoryPolicy::Off, now).is_empty());
}

#[test]
fn join_history_protected_rows_do_not_consume_count_or_byte_budgets() {
    let ordinary = HistoryEntry {
        id: EventId::new([1; 32]),
        from_me: false,
        who: "author".into(),
        text: b"eligible ordinary text".to_vec(),
        protection: None,
        wall_clock: 1,
        reply_to: None,
        file: None,
        recalled: false,
        recalled_text: None,
        sticker: None,
        expires_at_ms: None,
    };
    for mode in [
        super::super::MessageProtection::Click,
        super::super::MessageProtection::Authenticate,
    ] {
        for policy in [HistoryPolicy::LastTen, HistoryPolicy::LastThreeDays] {
            let mut entries = vec![ordinary.clone()];
            for i in 2..=12 {
                let mut protected = ordinary.clone();
                protected.id = EventId::new([i; 32]);
                protected.protection = Some(mode);
                // Exercise the byte budget as well as default redacted empty rows.
                if policy == HistoryPolicy::LastThreeDays {
                    protected.text = vec![b's'; SNAPSHOT_BYTES / 2 - 256];
                } else {
                    protected.text.clear();
                }
                entries.push(protected);
            }
            let selected = select_history(entries, policy, 2);
            assert_eq!(
                selected.len(),
                1,
                "protected rows are ineligible for history"
            );
            assert_eq!(selected[0].id, ordinary.id);
        }
    }
}

#[tokio::test]
async fn join_history_foreign_protected_offer_preserves_concealed_receiver_projection() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let alice = node(a.path());
    let bob = node(b.path());
    let channel = alice
        .create_channel_invited("foreign history", vec![])
        .await
        .unwrap();
    let root = GroupKey::generate();
    let mut rows = Vec::new();
    let mut protections = HashMap::new();
    for protection in [
        super::super::MessageProtection::Click,
        super::super::MessageProtection::Authenticate,
    ] {
        let id = alice
            .enqueue_protected_channel_message_reply_expiring_if(
                channel,
                b"foreign protected secret",
                None,
                None,
                protection,
                |commit| commit(),
            )
            .unwrap();
        let body =
            MessageBody::protected(b"foreign protected secret".to_vec(), None, protection).encode();
        protections.insert(id, protection);
        rows.push(EncryptedRow {
            id,
            ciphertext: crate::channel::seal_channel_message(&row_key(&root, id), &body).unwrap(),
        });
    }
    alice
        .add_channel_member(channel, bob.identity.public())
        .await
        .unwrap();
    let invitation = alice
        .log
        .lock()
        .unwrap()
        .events(&channel)
        .into_iter()
        .find(|event| {
            event.kind == EventKind::MembershipChange
                && crate::channel::ChannelMeta::is_invitation(&event.ciphertext)
                && crate::channel::ChannelMeta::decode(&event.ciphertext)
                    .is_some_and(|meta| meta.is_member(&bob.user_id()))
        })
        .unwrap()
        .id;
    // A foreign owner can create an offer without using this client's selector.
    let offer = Offer {
        recipient: bob.user_id(),
        invitation,
        policy: HistoryPolicy::LastTenWithinThreeDays,
        keys: SealedPayload::seal(&alice.identity, &[alice.identity.public()], root.as_bytes())
            .unwrap(),
        rows,
    };
    alice
        .append_event(
            channel,
            EventKind::ChannelJoinHistory,
            encode(&HistoryControl::Offer(offer)),
        )
        .unwrap();
    copy(&alice, &bob, channel);
    bob.accept_channel_invite(channel).await.unwrap();
    copy(&bob, &alice, channel);
    alice.retry_join_history().unwrap();
    copy(&alice, &bob, channel);
    let log = bob.log.lock().unwrap();
    let messages = received(&bob.identity, channel, &log.events(&channel));
    assert_eq!(messages.len(), 2);
    for message in messages {
        assert_eq!(
            message.protection,
            protections.get(&message.event_id).copied(),
            "foreign protected copies retain their exact reveal policy"
        );
        assert!(
            message.text.is_empty(),
            "receiver callbacks never contain protected text"
        );
    }
    drop(log);
    let history = bob.channel_history(channel, 10);
    assert_eq!(history.len(), 2);
    assert!(
        history
            .iter()
            .all(|row| row.protection == protections.get(&row.id).copied() && row.text.is_empty())
    );
}

#[tokio::test]
async fn join_history_acceptance_before_offer_arrives_is_recoverable() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let alice = node(a.path());
    let bob = node(b.path());
    let channel = alice
        .create_channel_invited("history", vec![])
        .await
        .unwrap();
    alice
        .send_channel_message(channel, b"earlier")
        .await
        .unwrap();
    alice
        .add_channel_member(channel, bob.identity.public())
        .await
        .unwrap();
    let events = alice
        .log
        .lock()
        .unwrap()
        .events(&channel)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    for event in events
        .into_iter()
        .filter(|event| event.kind != EventKind::ChannelJoinHistory)
    {
        bob.log.lock().unwrap().append(event).unwrap();
    }
    bob.process_channel(channel);
    bob.accept_channel_invite(channel).await.unwrap();
    copy(&bob, &alice, channel);
    alice.retry_join_history().unwrap();
    copy(&alice, &bob, channel);
    assert_eq!(bob.channel_history(channel, 10)[0].text, b"earlier");
}

#[tokio::test]
async fn join_history_policy_change_does_not_expand_or_cancel_frozen_invitation() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let alice = node(a.path());
    let bob = node(b.path());
    let channel = alice
        .create_channel_invited("history", vec![])
        .await
        .unwrap();
    alice
        .send_channel_message(channel, b"earlier")
        .await
        .unwrap();
    alice
        .add_channel_member(channel, bob.identity.public())
        .await
        .unwrap();
    alice
        .set_channel_history_policy(channel, HistoryPolicy::Off)
        .await
        .unwrap();
    copy(&alice, &bob, channel);
    bob.accept_channel_invite(channel).await.unwrap();
    copy(&bob, &alice, channel);
    alice.retry_join_history().unwrap();
    copy(&alice, &bob, channel);
    assert_eq!(bob.channel_history(channel, 10).len(), 1);
    let c = tempfile::tempdir().unwrap();
    let carol = node(c.path());
    alice
        .add_channel_member(channel, carol.identity.public())
        .await
        .unwrap();
    copy(&alice, &carol, channel);
    carol.accept_channel_invite(channel).await.unwrap();
    copy(&carol, &alice, channel);
    alice.retry_join_history().unwrap();
    copy(&alice, &carol, channel);
    assert!(carol.channel_history(channel, 10).is_empty());
}

#[tokio::test]
async fn join_history_obsolete_offers_never_append_invalid_retry_releases() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let alice = node(a.path());
    let bob = node(b.path());
    let channel = alice
        .create_channel_invited("history", vec![])
        .await
        .unwrap();
    alice
        .send_channel_message(channel, b"earlier")
        .await
        .unwrap();
    alice
        .add_channel_member(channel, bob.identity.public())
        .await
        .unwrap();
    alice
        .remove_channel_member(channel, &bob.user_id())
        .await
        .unwrap();
    alice
        .add_channel_member(channel, bob.identity.public())
        .await
        .unwrap();
    copy(&alice, &bob, channel);
    bob.accept_channel_invite(channel).await.unwrap();
    copy(&bob, &alice, channel);
    alice.retry_join_history().unwrap();
    let count = alice.log.lock().unwrap().events(&channel).len();
    for _ in 0..3 {
        alice.retry_join_history().unwrap();
    }
    assert_eq!(alice.log.lock().unwrap().events(&channel).len(), count);
    copy(&alice, &bob, channel);
    assert_eq!(bob.channel_history(channel, 10).len(), 1);
}

#[tokio::test]
async fn join_history_authorized_replica_releases_without_owner_and_survives_restart() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let c = tempfile::tempdir().unwrap();
    let alice = node(a.path());
    let bob = node(b.path());
    let carol = node(c.path());
    let channel = alice
        .create_channel("history", vec![bob.identity.public()])
        .await
        .unwrap();
    alice
        .set_channel_history_policy(channel, HistoryPolicy::LastTen)
        .await
        .unwrap();
    alice
        .send_channel_message(channel, b"earlier")
        .await
        .unwrap();
    copy(&alice, &bob, channel);
    alice
        .add_channel_member(channel, carol.identity.public())
        .await
        .unwrap();
    copy(&alice, &bob, channel);
    copy(&alice, &carol, channel);
    carol.accept_channel_invite(channel).await.unwrap();
    copy(&carol, &bob, channel);
    drop(alice);
    bob.set_channel_replica_enabled(channel, false)
        .await
        .unwrap();
    bob.retry_join_history().unwrap();
    copy(&bob, &carol, channel);
    assert!(carol.channel_history(channel, 10).is_empty());
    bob.set_channel_replica_enabled(channel, true)
        .await
        .unwrap();
    bob.retry_join_history().unwrap();
    copy(&bob, &carol, channel);
    assert_eq!(carol.channel_history(channel, 10)[0].text, b"earlier");
    let (ed, x) = carol.identity.secret_bytes();
    drop(carol);
    let (dm, _) = tokio::sync::mpsc::unbounded_channel();
    let (ch, _) = tokio::sync::mpsc::unbounded_channel();
    let (file, _) = tokio::sync::mpsc::unbounded_channel();
    let reopened = Node::open(
        DeviceIdentity::from_secret_bytes(ed, x),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        ch,
        file,
        &c.path().join("events"),
        &c.path().join("sent"),
        "pw",
    )
    .unwrap();
    assert_eq!(reopened.channel_history(channel, 10).len(), 1);
    copy(&bob, &reopened, channel);
    assert_eq!(reopened.channel_history(channel, 10).len(), 1);
}

#[tokio::test]
async fn join_history_does_not_grant_existing_late_members_access_as_sources() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let c = tempfile::tempdir().unwrap();
    let alice = node(a.path());
    let bob = node(b.path());
    let carol = node(c.path());
    let channel = alice
        .create_channel_invited("history", vec![])
        .await
        .unwrap();
    alice
        .send_channel_message(channel, b"secret earlier")
        .await
        .unwrap();
    alice
        .set_channel_history_policy(channel, HistoryPolicy::Off)
        .await
        .unwrap();
    alice
        .add_channel_member(channel, bob.identity.public())
        .await
        .unwrap();
    copy(&alice, &bob, channel);
    bob.accept_channel_invite(channel).await.unwrap();
    copy(&bob, &alice, channel);
    alice
        .set_channel_history_policy(channel, HistoryPolicy::LastTen)
        .await
        .unwrap();
    alice
        .add_channel_member(channel, carol.identity.public())
        .await
        .unwrap();
    copy(&alice, &bob, channel);
    let log = bob.log.lock().unwrap();
    let refs = log.events(&channel);
    let view = View::new(&refs);
    for (event, offer) in view.offers() {
        assert!(
            !offer
                .keys
                .entries
                .iter()
                .any(|entry| entry.user_id == bob.user_id())
        );
        let author = view.author(event).unwrap();
        assert!(offer.keys.open(&bob.identity, &author.x25519_pub).is_none());
    }
    drop(log);
    assert!(bob.channel_history(channel, 10).is_empty());
}

#[tokio::test]
async fn join_history_expiry_and_recall_are_rechecked_before_key_release() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let alice = node(a.path());
    let bob = node(b.path());
    let channel = alice
        .create_channel_invited("history", vec![])
        .await
        .unwrap();
    alice.send_channel_message(channel, b"keep").await.unwrap();
    alice
        .send_channel_message(channel, b"recall before acceptance")
        .await
        .unwrap();
    let recall = alice.channel_history(channel, 10).last().unwrap().id;
    let deadline = super::super::node::now_millis() + 2000;
    alice
        .send_channel_message_reply_expiring(
            channel,
            b"expires while invitation is pending",
            None,
            Some(deadline),
        )
        .await
        .unwrap();
    alice
        .add_channel_member(channel, bob.identity.public())
        .await
        .unwrap();
    alice.recall_channel(channel, recall).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(
        deadline.saturating_sub(super::super::node::now_millis()) + 1,
    ))
    .await;
    copy(&alice, &bob, channel);
    bob.accept_channel_invite(channel).await.unwrap();
    copy(&bob, &alice, channel);
    alice.retry_join_history().unwrap();
    copy(&alice, &bob, channel);
    let history = bob.channel_history(channel, 10);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].text, b"keep");
    // Deadlines and future timestamps also fail closed in the selection owner.
    let mut expired = history[0].clone();
    expired.expires_at_ms = Some(1);
    let mut future = history[0].clone();
    future.wall_clock = u64::MAX;
    assert!(
        select_history(
            vec![expired, future],
            HistoryPolicy::LastTen,
            super::super::node::now_millis()
        )
        .is_empty()
    );
}

#[tokio::test]
async fn join_history_round_trip_over_noise_loopback_preserves_silent_provenance() {
    use crate::discovery::Announce;
    use std::net::{IpAddr, Ipv4Addr};
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let alice = node(a.path());
    let identity = DeviceIdentity::generate();
    let (dm, _) = tokio::sync::mpsc::unbounded_channel();
    let (ch, mut received) = tokio::sync::mpsc::unbounded_channel();
    let (file, _) = tokio::sync::mpsc::unbounded_channel();
    let bob = Node::open(
        identity,
        Arc::new(Mutex::new(Roster::default())),
        dm,
        ch,
        file,
        &b.path().join("events"),
        &b.path().join("sent"),
        "pw",
    )
    .unwrap();
    let a_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let b_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    alice.roster.lock().unwrap().update(
        &Announce::new(
            &bob.identity,
            "Bob",
            b_listener.local_addr().unwrap().port(),
        ),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &alice.user_id(),
    );
    bob.roster.lock().unwrap().update(
        &Announce::new(
            &alice.identity,
            "Alice",
            a_listener.local_addr().unwrap().port(),
        ),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &bob.user_id(),
    );
    let a_task = tokio::spawn(Arc::clone(&alice).run_accept_loop(a_listener));
    let b_task = tokio::spawn(Arc::clone(&bob).run_accept_loop(b_listener));
    let channel = alice
        .create_channel_invited("history", vec![])
        .await
        .unwrap();
    alice
        .send_channel_message(channel, b"earlier over loopback")
        .await
        .unwrap();
    let mut protected_ids = Vec::new();
    let image = a.path().join("private-image.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\nprivate image pixels").unwrap();
    for protection in [
        super::super::MessageProtection::Click,
        super::super::MessageProtection::Authenticate,
    ] {
        protected_ids.push(
            alice
                .enqueue_protected_channel_message_reply_expiring_if(
                    channel,
                    b"private history text",
                    None,
                    None,
                    protection,
                    |commit| commit(),
                )
                .unwrap(),
        );
        let (id, _) = alice
            .enqueue_protected_file_channel_progress_expiring_if(
                channel,
                &image,
                crate::file::FileKind::Media,
                |_| {},
                None,
                protection,
                |commit| commit(),
            )
            .await
            .unwrap();
        protected_ids.push(id);
    }
    alice
        .add_channel_member(channel, bob.identity.public())
        .await
        .unwrap();
    {
        let log = alice.log.lock().unwrap();
        let events = log.events(&channel);
        let view = View::new(&events);
        let offers = view.offers();
        assert_eq!(offers.len(), 1);
        assert_eq!(
            offers[0].1.rows.len(),
            1,
            "only ordinary text enters the encrypted offer"
        );
        assert!(
            offers[0]
                .1
                .rows
                .iter()
                .all(|row| !protected_ids.contains(&row.id))
        );
    }
    assert!(bob.channel_history(channel, 10).is_empty());
    bob.accept_channel_invite(channel).await.unwrap();
    alice.retry_channel_controls().await;
    let message = tokio::time::timeout(std::time::Duration::from_secs(5), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.text, b"earlier over loopback");
    assert_eq!(message.arrival_kind, "history_sync");
    let history = bob.channel_history(channel, 10);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, message.event_id);
    assert_eq!(history[0].protection, None);
    assert_eq!(message.protection, None);
    assert!(history.iter().all(|row| !protected_ids.contains(&row.id)));
    assert!(
        received.try_recv().is_err(),
        "protected snapshots never surface as ordinary messages"
    );
    a_task.abort();
    b_task.abort();
}

#[tokio::test]
async fn join_history_transferred_owner_can_invite_before_first_send() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let c = tempfile::tempdir().unwrap();
    let alice = node(a.path());
    let bob = node(b.path());
    let carol = node(c.path());
    let channel = alice
        .create_channel_invited("history", vec![bob.identity.public()])
        .await
        .unwrap();
    copy(&alice, &bob, channel);
    bob.accept_channel_invite(channel).await.unwrap();
    copy(&bob, &alice, channel);
    alice
        .transfer_channel_owner(channel, &bob.user_id())
        .await
        .unwrap();
    copy(&alice, &bob, channel);
    assert!(
        bob.channels
            .lock()
            .unwrap()
            .state(&channel)
            .unwrap()
            .export_my_senders()
            .is_empty()
    );
    bob.add_channel_member(channel, carol.identity.public())
        .await
        .unwrap();
    copy(&bob, &carol, channel);
    assert!(
        carol
            .channel_pending_members(channel)
            .contains(&carol.user_id())
    );
}

#[tokio::test]
async fn join_history_leave_then_rejoin_off_closes_the_original_permission() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let alice = node(a.path());
    let bob = node(b.path());
    let channel = alice
        .create_channel_invited("history", vec![])
        .await
        .unwrap();
    alice
        .send_channel_message(channel, b"earlier")
        .await
        .unwrap();
    alice
        .add_channel_member(channel, bob.identity.public())
        .await
        .unwrap();
    copy(&alice, &bob, channel);
    bob.accept_channel_invite(channel).await.unwrap();
    copy(&bob, &alice, channel);
    bob.leave_channel(channel).await.unwrap();
    copy(&bob, &alice, channel);
    alice
        .set_channel_history_policy(channel, HistoryPolicy::Off)
        .await
        .unwrap();
    alice
        .add_channel_member(channel, bob.identity.public())
        .await
        .unwrap();
    copy(&alice, &bob, channel);
    bob.accept_channel_invite(channel).await.unwrap();
    copy(&bob, &alice, channel);
    let count = alice.log.lock().unwrap().events(&channel).len();
    alice.retry_join_history().unwrap();
    assert_eq!(alice.log.lock().unwrap().events(&channel).len(), count);
    copy(&alice, &bob, channel);
    assert!(bob.channel_history(channel, 10).is_empty());
}

#[test]
fn join_history_snapshot_budget_counts_sticker_metadata_and_skips_large_rows() {
    let row = HistoryEntry {
        id: EventId::new([1; 32]),
        from_me: false,
        who: "author".into(),
        text: b"ok".to_vec(),
        protection: None,
        wall_clock: 1,
        reply_to: None,
        file: None,
        recalled: false,
        recalled_text: None,
        sticker: None,
        expires_at_ms: None,
    };
    let mut oversized = row.clone();
    oversized.id = EventId::new([2; 32]);
    oversized.sticker = Some("x".repeat(SNAPSHOT_BYTES));
    let selected = select_history(vec![row, oversized], HistoryPolicy::LastTen, 2);
    assert_eq!(selected.len(), 1);
    assert!(selected[0].sticker.is_none());
}

#[tokio::test]
async fn join_history_signed_nonowner_policy_is_ignored_by_replay() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let alice = node(a.path());
    let bob = node(b.path());
    let channel = alice
        .create_channel("history", vec![bob.identity.public()])
        .await
        .unwrap();
    copy(&alice, &bob, channel);
    bob.append_event(
        channel,
        EventKind::ChannelHistoryPolicy,
        encode(&HistoryPolicy::LastTen),
    )
    .unwrap();
    copy(&bob, &alice, channel);
    assert_eq!(alice.channel_history_policy(channel), HistoryPolicy::Off);
    let mut malformed = encode(&HistoryPolicy::LastTen);
    malformed.push(0);
    assert!(decode::<HistoryPolicy>(&malformed).is_none());
    assert!(decode::<HistoryPolicy>(&[255; 4]).is_none());
}

#[tokio::test]
async fn join_history_three_day_cutoff_checks_original_signed_times() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let alice = node(a.path());
    let bob = node(b.path());
    let channel = alice
        .create_channel_invited("history", vec![])
        .await
        .unwrap();
    for (age, text) in [
        (4 * DAY_MS, b"too old".as_slice()),
        (DAY_MS, b"recent".as_slice()),
    ] {
        let plaintext = MessageBody::new(text.to_vec(), None).encode();
        let ciphertext = alice
            .channels
            .lock()
            .unwrap()
            .state_mut(&channel)
            .unwrap()
            .seal_sender_message(&plaintext)
            .unwrap();
        let wall_clock = super::super::node::now_millis() - age;
        let mut log = alice.log.lock().unwrap();
        let (parents, lamport) = log.prepare(&channel).unwrap();
        let seq = log
            .next_sequence(
                &channel,
                &crate::eventlog::Author::from_ed25519(alice.identity.public().ed25519_pub),
            )
            .unwrap();
        log.append(Event::new(
            &alice.identity,
            channel,
            seq,
            parents,
            lamport,
            wall_clock,
            EventKind::Message,
            ciphertext,
        ))
        .unwrap();
        drop(log);
        alice
            .sentlog
            .lock()
            .unwrap()
            .record(channel, seq, wall_clock, &plaintext)
            .unwrap();
    }
    alice
        .add_channel_member(channel, bob.identity.public())
        .await
        .unwrap();
    copy(&alice, &bob, channel);
    bob.accept_channel_invite(channel).await.unwrap();
    copy(&bob, &alice, channel);
    alice.retry_join_history().unwrap();
    copy(&alice, &bob, channel);
    let history = bob.channel_history(channel, 10);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].text, b"recent");
}

#[tokio::test]
async fn join_history_former_owner_fork_cannot_override_current_owner_policy() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let alice = node(a.path());
    let bob = node(b.path());
    let channel = alice
        .create_channel("history", vec![bob.identity.public()])
        .await
        .unwrap();
    let (old_parents, _) = alice.log.lock().unwrap().prepare(&channel).unwrap();
    alice
        .transfer_channel_owner(channel, &bob.user_id())
        .await
        .unwrap();
    let mut log = alice.log.lock().unwrap();
    let (_, lamport) = log.prepare(&channel).unwrap();
    let seq = log
        .next_sequence(
            &channel,
            &crate::eventlog::Author::from_ed25519(alice.identity.public().ed25519_pub),
        )
        .unwrap();
    log.append(Event::new(
        &alice.identity,
        channel,
        seq,
        old_parents,
        lamport + 1,
        super::super::node::now_millis(),
        EventKind::ChannelHistoryPolicy,
        encode(&HistoryPolicy::LastTen),
    ))
    .unwrap();
    drop(log);
    alice.process_channel(channel);
    assert_eq!(alice.channel_owner(channel), bob.user_id());
    assert_eq!(alice.channel_history_policy(channel), HistoryPolicy::Off);
}

#[tokio::test]
async fn join_history_former_owner_cannot_backdate_a_policy_into_a_closed_owner_lifetime() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let alice = node(a.path());
    let bob = node(b.path());
    let channel = alice
        .create_channel("history", vec![bob.identity.public()])
        .await
        .unwrap();
    let (old_parents, _) = alice.log.lock().unwrap().prepare(&channel).unwrap();
    alice
        .transfer_channel_owner(channel, &bob.user_id())
        .await
        .unwrap();
    let mut log = alice.log.lock().unwrap();
    let seq = log
        .next_sequence(
            &channel,
            &crate::eventlog::Author::from_ed25519(alice.identity.public().ed25519_pub),
        )
        .unwrap();
    log.append(Event::new(
        &alice.identity,
        channel,
        seq,
        old_parents,
        1,
        super::super::node::now_millis(),
        EventKind::ChannelHistoryPolicy,
        encode(&HistoryPolicy::LastTen),
    ))
    .unwrap();
    drop(log);
    alice.process_channel(channel);
    assert_eq!(alice.channel_owner(channel), bob.user_id());
    assert_eq!(alice.channel_history_policy(channel), HistoryPolicy::Off);
}

#[tokio::test]
async fn join_history_rejoined_source_cannot_use_old_ancestry_to_read_removal_period() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let c = tempfile::tempdir().unwrap();
    let alice = node(a.path());
    let bob = node(b.path());
    let carol = node(c.path());
    let channel = alice
        .create_channel("history", vec![bob.identity.public()])
        .await
        .unwrap();
    let (old_parents, _) = alice.log.lock().unwrap().prepare(&channel).unwrap();
    alice
        .remove_channel_member(channel, &bob.user_id())
        .await
        .unwrap();
    alice
        .distribute_my_sender_key(channel, &[alice.identity.public()])
        .unwrap();
    let body = MessageBody::new(b"sent while Bob was removed".to_vec(), None).encode();
    let ciphertext = alice
        .channels
        .lock()
        .unwrap()
        .state_mut(&channel)
        .unwrap()
        .seal_sender_message(&body)
        .unwrap();
    let now = super::super::node::now_millis();
    let seq = {
        let mut log = alice.log.lock().unwrap();
        let (_, clock) = log.prepare(&channel).unwrap();
        let seq = log
            .next_sequence(
                &channel,
                &crate::eventlog::Author::from_ed25519(alice.identity.public().ed25519_pub),
            )
            .unwrap();
        log.append(Event::new(
            &alice.identity,
            channel,
            seq,
            old_parents,
            clock + 10,
            now,
            EventKind::Message,
            ciphertext,
        ))
        .unwrap();
        seq
    };
    alice
        .sentlog
        .lock()
        .unwrap()
        .record(channel, seq, now, &body)
        .unwrap();
    alice
        .add_channel_member(channel, bob.identity.public())
        .await
        .unwrap();
    copy(&alice, &bob, channel);
    bob.accept_channel_invite(channel).await.unwrap();
    copy(&bob, &alice, channel);
    alice
        .set_channel_history_policy(channel, HistoryPolicy::LastTen)
        .await
        .unwrap();
    alice
        .add_channel_member(channel, carol.identity.public())
        .await
        .unwrap();
    let log = alice.log.lock().unwrap();
    let refs = log.events(&channel);
    let view = View::new(&refs);
    assert_eq!(view.offers().len(), 1);
    assert!(view.offers().iter().all(|(_, offer)| {
        !offer
            .keys
            .entries
            .iter()
            .any(|entry| entry.user_id == bob.user_id())
    }));
}
