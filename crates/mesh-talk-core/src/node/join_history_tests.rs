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

fn copy_events(from: &Node, to: &Node, channel: ConversationId) {
    let events = from
        .log
        .lock()
        .unwrap()
        .events(&channel)
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>();
    for event in events {
        to.log.lock().unwrap().append(event).unwrap();
    }
}

fn copy(from: &Node, to: &Node, channel: ConversationId) {
    copy_events(from, to, channel);
    to.process_channel(channel);
    settle_history(to);
}

fn settle_history(node: &Node) -> Vec<ConversationId> {
    let mut ready = Vec::new();
    for _ in 0..30000 {
        node.retry_channel_plaintext();
        let released = match node.retry_join_history() {
            Ok(released) => released,
            // A release can advance the source while another bounded history
            // continuation is still attached to the previous checkpoint.
            Err(NodeError::Log(crate::eventlog::LogError::StaleCursor)) => continue,
            Err(_) => panic!("history retry failed"),
        };
        for channel in released {
            if !ready.contains(&channel) {
                ready.push(channel);
            }
        }
        if node.join_history_work.lock().unwrap().is_idle()
            && node
                .channel_checkpoints
                .next_runtime(None)
                .unwrap()
                .is_none()
        {
            return ready;
        }
    }
    panic!("bounded history retry turns did not settle");
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
        alice.channel_history_policy(channel).unwrap(),
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
        bob.channel_history(channel, 100).unwrap().is_empty(),
        "pending invite must not decrypt a history offer"
    );
    bob.accept_channel_invite(channel).await.unwrap();
    copy(&bob, &alice, channel);
    settle_history(&alice);
    let before = alice
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .body_read_counts();
    for _ in 0..3 {
        settle_history(&alice);
    }
    assert_eq!(
        alice
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts(),
        before,
        "completed sender retry must reuse the unchanged control prefix"
    );

    copy(&alice, &bob, channel);
    let history = bob.channel_history(channel, 100).unwrap();
    assert_eq!(history.len(), 10);
    assert_eq!(history[0].text, b"message 4");
    assert_eq!(history[9].text, b"message 13");
    copy(&alice, &bob, channel);
    assert_eq!(
        bob.channel_history(channel, 100).unwrap().len(),
        10,
        "delivery replay must deduplicate"
    );
    assert_eq!(bob.reception_effects.head().unwrap().sequence, 10);
    let before = bob
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .body_read_counts();
    bob.process_channel(channel);
    assert_eq!(
        bob.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts(),
        before,
        "completed invitation history must not reopen historical controls"
    );
    assert_eq!(bob.reception_effects.head().unwrap().sequence, 10);
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
    assert_eq!(
        alice.channel_history_policy(channel).unwrap(),
        HistoryPolicy::Off
    );
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
    assert_eq!(
        bob.channel_history_policy(channel).unwrap(),
        HistoryPolicy::LastTen
    );
}

#[test]
fn join_history_time_and_count_bounds() {
    let now = 10 * DAY_MS;
    let entries = (0..15)
        .map(|i| HistoryEntry {
            file_message_aliases: Vec::new(),
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
        file_message_aliases: Vec::new(),
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
        .unwrap()
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
    settle_history(&alice);
    copy(&alice, &bob, channel);
    let log = bob.log.lock().unwrap();
    let messages = received(
        &bob.identity,
        channel,
        &log.events(&channel).unwrap().iter().collect::<Vec<_>>(),
    )
    .unwrap();
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
    let history = bob.channel_history(channel, 10).unwrap();
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
        .unwrap()
        .into_iter()
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
    settle_history(&alice);
    copy(&alice, &bob, channel);
    assert_eq!(
        bob.channel_history(channel, 10).unwrap()[0].text,
        b"earlier"
    );
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
    settle_history(&alice);
    copy(&alice, &bob, channel);
    assert_eq!(bob.channel_history(channel, 10).unwrap().len(), 1);
    let c = tempfile::tempdir().unwrap();
    let carol = node(c.path());
    alice
        .add_channel_member(channel, carol.identity.public())
        .await
        .unwrap();
    copy(&alice, &carol, channel);
    carol.accept_channel_invite(channel).await.unwrap();
    copy(&carol, &alice, channel);
    settle_history(&alice);
    copy(&alice, &carol, channel);
    assert!(carol.channel_history(channel, 10).unwrap().is_empty());
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
    settle_history(&alice);
    let count = alice.log.lock().unwrap().events(&channel).unwrap().len();
    for _ in 0..3 {
        settle_history(&alice);
    }
    assert_eq!(
        alice.log.lock().unwrap().events(&channel).unwrap().len(),
        count
    );
    copy(&alice, &bob, channel);
    assert_eq!(bob.channel_history(channel, 10).unwrap().len(), 1);
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
    drop(alice);
    // Disable the replica before delivering acceptance. The copy helper also
    // runs the real bounded release worker; an already-published release cannot
    // be revoked by changing sharing afterwards.
    bob.set_channel_replica_enabled(channel, false)
        .await
        .unwrap();
    copy(&carol, &bob, channel);
    settle_history(&bob);
    copy(&bob, &carol, channel);
    assert!(carol.channel_history(channel, 10).unwrap().is_empty());
    bob.set_channel_replica_enabled(channel, true)
        .await
        .unwrap();
    settle_history(&bob);
    copy(&bob, &carol, channel);
    assert_eq!(
        carol.channel_history(channel, 10).unwrap()[0].text,
        b"earlier"
    );
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
    assert_eq!(reopened.channel_history(channel, 10).unwrap().len(), 1);
    copy(&bob, &reopened, channel);
    assert_eq!(reopened.channel_history(channel, 10).unwrap().len(), 1);
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
    let refs = log.events(&channel).unwrap();
    let view = View::new(&refs.iter().collect::<Vec<_>>());
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
    assert!(bob.channel_history(channel, 10).unwrap().is_empty());
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
    let recall = alice
        .channel_history(channel, 10)
        .unwrap()
        .last()
        .unwrap()
        .id;
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
    settle_history(&alice);
    copy(&alice, &bob, channel);
    let history = bob.channel_history(channel, 10).unwrap();
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
        &Announce::new_with_account(
            &bob.identity,
            &bob.account,
            "Bob",
            b_listener.local_addr().unwrap().port(),
        ),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &alice.user_id(),
    );
    bob.roster.lock().unwrap().update(
        &Announce::new_with_account(
            &alice.identity,
            &alice.account,
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
        let events = log.events(&channel).unwrap();
        let view = View::new(&events.iter().collect::<Vec<_>>());
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
    assert!(bob.channel_history(channel, 10).unwrap().is_empty());
    bob.accept_channel_invite(channel).await.unwrap();
    alice.retry_channel_controls().await;
    let message = tokio::time::timeout(std::time::Duration::from_secs(5), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.text, b"earlier over loopback");
    assert_eq!(message.arrival_kind, "history_sync");
    let history = bob.channel_history(channel, 10).unwrap();
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
    // This scenario closes permission before the original key is released.
    // Stage acceptance and departure authority without running the sender's
    // global history retry, which could legitimately release the first offer.
    copy_events(&bob, &alice, channel);
    crate::node::test_support::settle_group_metadata(&alice, channel);
    let release_count = || {
        alice
            .log
            .lock()
            .unwrap()
            .events(&channel)
            .unwrap()
            .iter()
            .filter(|event| {
                event.kind == EventKind::ChannelJoinHistory
                    && matches!(
                        decode::<HistoryControl>(&event.ciphertext),
                        Some(HistoryControl::Release(_))
                    )
            })
            .count()
    };
    assert_eq!(release_count(), 0, "the original key must remain withheld");
    assert!(bob.channel_history(channel, 10).unwrap().is_empty());
    bob.leave_channel(channel).await.unwrap();
    copy_events(&bob, &alice, channel);
    crate::node::test_support::settle_group_metadata(&alice, channel);
    assert_eq!(release_count(), 0);
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
    let count = alice.log.lock().unwrap().events(&channel).unwrap().len();
    settle_history(&alice);
    assert_eq!(
        alice.log.lock().unwrap().events(&channel).unwrap().len(),
        count
    );
    assert_eq!(
        release_count(),
        0,
        "a closed grant cannot release its old key"
    );
    copy(&alice, &bob, channel);
    assert!(bob.channel_history(channel, 10).unwrap().is_empty());
}

#[test]
fn join_history_snapshot_budget_counts_sticker_metadata_and_skips_large_rows() {
    let row = HistoryEntry {
        file_message_aliases: Vec::new(),
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
    assert_eq!(
        alice.channel_history_policy(channel).unwrap(),
        HistoryPolicy::Off
    );
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
    settle_history(&alice);
    copy(&alice, &bob, channel);
    let history = bob.channel_history(channel, 10).unwrap();
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
    // The forged signed append still needs bounded policy preparation before
    // observing its rejection; this fixture has no participant worker.
    crate::node::test_support::settle_group_metadata(&alice, channel);
    assert_eq!(alice.channel_owner(channel), bob.user_id());
    assert_eq!(
        alice.channel_history_policy(channel).unwrap(),
        HistoryPolicy::Off
    );
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
    // The forged signed append still needs bounded policy preparation before
    // observing its rejection; this fixture has no participant worker.
    crate::node::test_support::settle_group_metadata(&alice, channel);
    assert_eq!(alice.channel_owner(channel), bob.user_id());
    assert_eq!(
        alice.channel_history_policy(channel).unwrap(),
        HistoryPolicy::Off
    );
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
    let refs = log.events(&channel).unwrap();
    let view = View::new(&refs.iter().collect::<Vec<_>>());
    assert_eq!(view.offers().len(), 1);
    assert!(view.offers().iter().all(|(_, offer)| {
        !offer
            .keys
            .entries
            .iter()
            .any(|entry| entry.user_id == bob.user_id())
    }));
}

#[tokio::test]
async fn account_invitation_history_reaches_accepting_sibling_while_anchor_is_offline() {
    use super::super::channel_account_authority::{AccountAuthorityControl, AccountGrant};
    use super::super::channel_account_bootstrap::{DeviceControl, inheritance_account_message};
    use crate::identity::account::Account;
    let owner_dir = tempfile::tempdir().unwrap();
    let anchor_dir = tempfile::tempdir().unwrap();
    let sibling_dir = tempfile::tempdir().unwrap();
    let owner = node(owner_dir.path());
    let anchor = node(anchor_dir.path());
    let (dm, _) = tokio::sync::mpsc::unbounded_channel();
    let (channels, _) = tokio::sync::mpsc::unbounded_channel();
    let (files, _) = tokio::sync::mpsc::unbounded_channel();
    let sibling = Node::open_with_account(
        DeviceIdentity::generate(),
        Account::from_secret_bytes(anchor.account.secret_bytes()),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channels,
        files,
        &sibling_dir.path().join("events"),
        &sibling_dir.path().join("sent"),
        "pw",
    )
    .unwrap();
    let channel = owner
        .create_channel_invited("account invitation history", vec![])
        .await
        .unwrap();
    for index in 0..14 {
        owner
            .send_channel_message(channel, format!("frozen row {index}").as_bytes())
            .await
            .unwrap();
    }
    let root = owner
        .log
        .lock()
        .unwrap()
        .events(&channel)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == EventKind::MembershipChange)
        .unwrap()
        .id;
    let proof = owner.own_group_proof();
    let control = AccountAuthorityControl {
        version: 1,
        previous: None,
        issuer: proof.clone(),
        owner_account: owner.account.public().ed25519_pub,
        grants: vec![AccountGrant {
            incarnation: [1; 16],
            pending: false,
            original: proof.clone(),
            membership_event: root,
            acceptance_event: None,
        }],
        revoked: vec![],
        legacy_bindings: vec![proof],
        account_signature: vec![],
    };
    owner
        .append_event(
            channel,
            EventKind::ChannelAccountAuthority,
            control.signed_encode(channel, &owner.account),
        )
        .unwrap();
    crate::node::test_support::settle_group_metadata(&owner, channel);
    owner
        .try_account_channel_invite(
            channel,
            &anchor.identity.public(),
            &anchor.own_group_proof(),
        )
        .unwrap();
    copy(&owner, &sibling, channel);
    assert!(sibling.channel_history(channel, 100).unwrap().is_empty());
    let projection = sibling.account_group_projection(channel).unwrap();
    let authority = projection.authority.current_event().unwrap();
    let grant = projection
        .authority
        .account_grant(&sibling.account.public().ed25519_pub)
        .unwrap()
        .id()
        .unwrap();
    let inherit = DeviceControl::Inherit {
        authority,
        grant,
        device: sibling.own_group_proof(),
        account_signature: sibling
            .account
            .sign(&inheritance_account_message(
                channel,
                authority,
                grant,
                &sibling.identity.public(),
            ))
            .to_vec(),
    };
    sibling
        .append_event(
            channel,
            EventKind::ChannelDeviceInheritance,
            inherit.encode(),
        )
        .unwrap();
    sibling.process_channel(channel);
    sibling.accept_channel_invite(channel).await.unwrap();
    copy(&sibling, &owner, channel);
    settle_history(&owner);
    copy(&owner, &sibling, channel);
    let history = sibling.channel_history(channel, 100).unwrap();
    assert_eq!(
        history.len(),
        10,
        "the accepting account device receives only the frozen selected history while its anchor never runs"
    );
    assert_eq!(history[0].text, b"frozen row 4");
    assert_eq!(history[9].text, b"frozen row 13");
    copy(&owner, &sibling, channel);
    assert_eq!(
        sibling.channel_history(channel, 100).unwrap().len(),
        10,
        "account release replay stays deduplicated"
    );
    let release_events = owner
        .log
        .lock()
        .unwrap()
        .events(&channel)
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>();
    let graph = release_events
        .iter()
        .map(|event| (event.id, event))
        .collect::<HashMap<_, _>>();
    let released = release_events
        .iter()
        .find(|event| {
            decode_account_release(&event.ciphertext)
                .is_some_and(|release| release.recipient == sibling.identity.public())
        })
        .unwrap();
    let projected = sibling.account_group_projection(channel).unwrap();
    let state = projected.state.as_ref().unwrap();
    assert!(account_release_admitted(state, released, &projected.authority, &graph).unwrap());
    for tamper in 0..3 {
        let mut body = decode_account_release(&released.ciphertext).unwrap();
        match tamper {
            0 => body.grant.incarnation[0] ^= 1,
            1 => body.recipient.x25519_pub = DeviceIdentity::generate().public().x25519_pub,
            _ => body.release.sealed_key = vec![0; SNAPSHOT_BYTES * 2 + 1],
        }
        let forged = Event::new(
            &owner.identity,
            channel,
            999,
            released.parents.clone(),
            999,
            999,
            EventKind::ChannelDeviceInheritance,
            encode_account_release(&body),
        );
        assert!(
            !account_release_admitted(state, &forged, &projected.authority, &graph).unwrap(),
            "signed source cannot substitute another grant, recipient key or oversized key payload"
        );
    }
    owner
        .remove_channel_account_members_if(channel, &anchor.account_id(), |commit| commit())
        .await
        .unwrap();
    let removed = owner.account_group_projection(channel).unwrap();
    assert!(
        !account_release_admitted(
            removed.state.as_ref().unwrap(),
            released,
            &removed.authority,
            &graph
        )
        .unwrap()
    );
    owner
        .try_account_channel_invite(
            channel,
            &anchor.identity.public(),
            &anchor.own_group_proof(),
        )
        .unwrap();
    // The explicit local invitation helper has no participant worker; finish
    // its metadata before checking the new grant against the old release.
    crate::node::test_support::settle_group_metadata(&owner, channel);
    let reinvited = owner.account_group_projection(channel).unwrap();
    assert!(
        !account_release_admitted(
            reinvited.state.as_ref().unwrap(),
            released,
            &reinvited.authority,
            &graph
        )
        .unwrap(),
        "a new account invitation cannot reuse an earlier incarnation's selected history permission"
    );
}

#[tokio::test]
async fn account_invitation_three_event_wal_recovers_atomically_after_restart() {
    use super::super::channel_account_authority::{AccountAuthorityControl, AccountGrant};
    use super::super::delivery_store::DeliveryTransaction;
    use super::super::sentlog::SentEntry;
    use crate::identity::account::Account;
    let directory = tempfile::tempdir().unwrap();
    let target_dir = tempfile::tempdir().unwrap();
    let staged = tempfile::tempdir().unwrap();
    let owner = node(directory.path());
    let target = node(target_dir.path());
    let channel = owner
        .create_channel_invited("atomic account invitation", vec![])
        .await
        .unwrap();
    owner
        .send_channel_message(channel, b"one frozen history row")
        .await
        .unwrap();
    let root = owner
        .log
        .lock()
        .unwrap()
        .events(&channel)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == EventKind::MembershipChange)
        .unwrap()
        .id;
    let proof = owner.own_group_proof();
    let control = AccountAuthorityControl {
        version: 1,
        previous: None,
        issuer: proof.clone(),
        owner_account: owner.account.public().ed25519_pub,
        grants: vec![AccountGrant {
            incarnation: [1; 16],
            pending: false,
            original: proof.clone(),
            membership_event: root,
            acceptance_event: None,
        }],
        revoked: vec![],
        legacy_bindings: vec![proof],
        account_signature: vec![],
    };
    owner
        .append_event(
            channel,
            EventKind::ChannelAccountAuthority,
            control.signed_encode(channel, &owner.account),
        )
        .unwrap();
    crate::node::test_support::settle_group_metadata(&owner, channel);
    owner
        .try_account_channel_invite(
            channel,
            &target.identity.public(),
            &target.own_group_proof(),
        )
        .unwrap();
    crate::node::test_support::settle_group_metadata(&owner, channel);
    let all = owner
        .log
        .lock()
        .unwrap()
        .events(&channel)
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>();
    let projection = owner.account_group_projection(channel).unwrap();
    let authority = all
        .iter()
        .find(|event| Some(event.id) == projection.authority.current_event())
        .unwrap()
        .clone();
    let invitation = all
        .iter()
        .find(|event| authority.parents.contains(&event.id))
        .unwrap()
        .clone();
    let offer = all.iter().find(|event| matches!(decode::<HistoryControl>(&event.ciphertext),Some(HistoryControl::Offer(offer)) if offer.invitation == invitation.id)).unwrap().clone();
    let transaction = vec![invitation.clone(), authority, offer.clone()];
    assert!(valid_invitation_transaction(&transaction));
    let identity_secret = owner.identity.secret_bytes();
    let account_secret = owner.account.secret_bytes();
    let open = || {
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (channels, _) = tokio::sync::mpsc::unbounded_channel();
        let (files, _) = tokio::sync::mpsc::unbounded_channel();
        Node::open_with_account(
            DeviceIdentity::from_secret_bytes(identity_secret.0, identity_secret.1),
            Account::from_secret_bytes(account_secret),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channels,
            files,
            &staged.path().join("events"),
            &staged.path().join("sent"),
            "pw",
        )
        .unwrap()
    };
    let before_crash = open();
    {
        let mut log = before_crash.log.lock().unwrap();
        for event in all.iter().filter(|event| event.seq < invitation.seq) {
            log.append(event.clone()).unwrap();
        }
        log.sync().unwrap();
    }
    before_crash.process_channel(channel);
    let senders = owner
        .channels
        .lock()
        .unwrap()
        .state(&channel)
        .unwrap()
        .export_my_senders();
    before_crash
        .delivery
        .lock()
        .unwrap()
        .begin(DeliveryTransaction::ChannelOutgoing {
            events: transaction.clone(),
            senders,
            sent: SentEntry {
                conversation: channel,
                seq: offer.seq,
                wall_clock: offer.wall_clock,
                plaintext: Vec::new(),
            },
        })
        .unwrap();
    assert!(transaction.iter().all(|event| {
        before_crash
            .log
            .lock()
            .unwrap()
            .get(&event.id)
            .unwrap()
            .is_none()
    }));
    drop(before_crash);
    let recovered = open();
    assert!(
        transaction.iter().all(|event| recovered
            .log
            .lock()
            .unwrap()
            .get(&event.id)
            .unwrap()
            .is_some()),
        "one pending WAL record recovers invitation, account grant and frozen offer together"
    );
    // The exact signed WAL rows above are durable before any projection repair.
    // This bare reopened profile has no worker to finish its bounded metadata.
    crate::node::test_support::settle_group_metadata(&recovered, channel);
    let projection = recovered.account_group_projection(channel).unwrap();
    let grant = projection
        .authority
        .account_grant(&target.account.public().ed25519_pub)
        .unwrap();
    assert!(grant.pending);
    assert_eq!(grant.membership_event, invitation.id);
    assert!(
        !projection
            .authority
            .is_member(&target.identity.public(), &target.own_group_proof())
    );
}

#[tokio::test]
async fn join_history_rejected_release_is_not_reopened_by_safe_tail_control() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let alice = node(a.path());
    let bob = node(b.path());
    let channel = alice
        .create_channel_invited("bounded retry", vec![])
        .await
        .unwrap();
    alice
        .send_channel_message(channel, b"old row")
        .await
        .unwrap();
    alice
        .add_channel_member(channel, bob.identity.public())
        .await
        .unwrap();
    copy(&alice, &bob, channel);
    bob.accept_channel_invite(channel).await.unwrap();
    // Install acceptance without running Alice's release worker: this fixture
    // deliberately supplies the only release below with an invalid sealed key.
    copy_events(&bob, &alice, channel);
    crate::node::test_support::settle_group_metadata(&alice, channel);
    let offer = alice
        .log
        .lock()
        .unwrap()
        .events(&channel)
        .unwrap()
        .into_iter()
        .find(|event| {
            matches!(
                decode::<HistoryControl>(&event.ciphertext),
                Some(HistoryControl::Offer(_))
            )
        })
        .unwrap()
        .id;
    let acceptance = bob
        .log
        .lock()
        .unwrap()
        .events(&channel)
        .unwrap()
        .into_iter()
        .find(|event| {
            event.kind == EventKind::ChannelAccept && event.author.user_id() == bob.user_id()
        })
        .unwrap()
        .id;
    alice
        .append_event(
            channel,
            EventKind::ChannelJoinHistory,
            encode(&HistoryControl::Release(Release {
                offer,
                acceptance,
                sealed_key: vec![0; 8],
            })),
        )
        .unwrap();
    copy(&alice, &bob, channel);
    assert!(bob.channel_history(channel, 10).unwrap().is_empty());
    let job = bob
        .channel_checkpoints
        .source_binding(channel)
        .unwrap()
        .unwrap()
        .job;
    alice
        .rename_channel(channel, "new current name")
        .await
        .unwrap();
    for event in alice.log.lock().unwrap().events(&channel).unwrap() {
        bob.log.lock().unwrap().append(event).unwrap();
    }
    crate::node::test_support::settle_group_metadata(&bob, channel);
    bob.account_group_projection(channel).unwrap();
    assert_eq!(
        bob.channel_checkpoints
            .source_binding(channel)
            .unwrap()
            .unwrap()
            .job,
        job,
        "this fixture must remain in the same canonical order generation"
    );
    let before = bob
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .body_read_counts();
    bob.process_join_history_reception(channel, false).unwrap();
    assert_eq!(
        bob.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts(),
        before,
        "unchanged rejected ciphertext cannot become decryptable after a later rename"
    );
    assert_eq!(bob.reception_effects.head().unwrap().sequence, 0);
}

#[tokio::test]
async fn join_history_retry_without_live_offers_reads_no_historical_bodies() {
    for count in [1, 80] {
        let directory = tempfile::tempdir().unwrap();
        let alice = node(directory.path());
        let channel = alice.create_channel("no offers", vec![]).await.unwrap();
        for _ in 0..count {
            alice
                .enqueue_channel_message_reply_expiring_if(
                    channel,
                    b"history",
                    None,
                    None,
                    |commit| commit(),
                )
                .unwrap();
        }
        alice.account_group_projection(channel).unwrap();
        let before = alice
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        for _ in 0..3 {
            assert!(settle_history(&alice).is_empty());
        }
        assert_eq!(
            alice
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            before,
            "unchanged history retry scanned signed history at {count} messages"
        );
    }
}

#[tokio::test]
async fn join_history_rejected_release_backlog_yields_before_decoding_every_control() {
    let directory = tempfile::tempdir().unwrap();
    let alice = node(directory.path());
    let channel = alice
        .create_channel("pending history", vec![])
        .await
        .unwrap();
    for n in 0u64..129 {
        alice
            .append_event(
                channel,
                EventKind::ChannelJoinHistory,
                encode(&HistoryControl::Release(Release {
                    offer: EventId::new([7; 32]),
                    acceptance: EventId::new([8; 32]),
                    sealed_key: n.to_be_bytes().to_vec(),
                })),
            )
            .unwrap();
    }
    {
        let log = alice.log.lock().unwrap();
        while !alice
            .channel_checkpoints
            .project_step(log.repository().unwrap(), &alice.identity, channel)
            .unwrap()
        {}
    }
    let counts = || {
        alice
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts()
            .1
    };
    let before = counts();
    alice
        .process_join_history_reception(channel, false)
        .unwrap();
    assert!(
        counts() - before <= 64,
        "one reception turn decoded {} rejected controls",
        counts() - before
    );
    for _ in 0..1024 {
        let before = counts();
        let ready = alice
            .process_join_history_reception(channel, false)
            .unwrap();
        assert!(counts() - before <= 64);
        if ready {
            break;
        }
    }
    let before = counts();
    alice
        .process_join_history_reception(channel, false)
        .unwrap();
    assert_eq!(
        counts(),
        before,
        "completed rejected controls must remain quiescent"
    );
    assert_eq!(alice.reception_effects.head().unwrap().sequence, 0);
}
