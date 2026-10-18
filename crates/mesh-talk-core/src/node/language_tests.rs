use super::*;
use crate::{
    discovery::Announce,
    identity::{account::Account, device::DeviceIdentity},
};

#[tokio::test]
async fn language_policy_persists_and_blocks_before_enqueue() {
    let dir = tempfile::tempdir().unwrap();
    let peer = DeviceIdentity::generate();
    let account = Account::generate();
    let proof = Announce::new_with_account(&peer, &account, "peer", 9);
    let (node, _, _) = test_support::node_with_files(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let profile =
        profile::ProfilePayload::new_with_language(&account, None, 100, Some("en".into()));
    node.language_state
        .lock()
        .unwrap()
        .record_profile(&profile)
        .unwrap();
    assert!(
        !node
            .validate_dm_language(
                &account.account_id(),
                "你好，我们明天一起出去吃午饭吧。".as_bytes()
            )
            .unwrap()
            .allowed
    );
    assert!(
        node.enqueue_to_account(
            &account.account_id(),
            "你好，我们明天一起出去吃午饭吧。".as_bytes(),
            None
        )
        .await
        .is_err()
    );
    assert!(node.account_history(&account.account_id(), 10).is_empty());
    assert!(
        node.validate_dm_language(&account.account_id(), b"We should meet tomorrow for lunch.")
            .unwrap()
            .allowed
    );
}

#[tokio::test]
async fn language_loopback_filters_unaware_sender_and_preserves_receipts_and_restart() {
    use std::time::Duration;
    use tokio::{net::TcpListener, time::timeout};
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_secret = bob.secret_bytes();
    let bob_account = Account::generate();
    let bob_account_secret = bob_account.secret_bytes();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
    let (a, _, _) = test_support::node_with_files(ad.path(), alice, aa, &bp);
    let (b, mut rx, _) = test_support::node_with_files(bd.path(), bob, bob_account, &ap);
    // The recipient enables a rule while this sender still has no cached policy.
    b.prepare_own_profile(None, Some(Some("en".into())))
        .unwrap();
    let original = "你好，我们明天一起出去吃午饭吧。";
    let id = a
        .enqueue_to_account(&b.account_id(), original.as_bytes(), None)
        .await
        .unwrap();
    let at = tokio::spawn(a.clone().run_accept_loop(al));
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    timeout(Duration::from_secs(8), a.flush_delivery(id))
        .await
        .unwrap();
    let message = timeout(Duration::from_secs(8), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.text, language_io::HIDDEN_TEXT);
    assert_eq!(
        b.account_history(&a.account_id(), 10)[0].text,
        language_io::HIDDEN_TEXT
    );
    assert!(b.search("午饭").is_empty());
    assert!(
        b.export_account_backfill().is_empty(),
        "hidden plaintext must not leak to a linked device"
    );
    timeout(Duration::from_secs(8), async {
        while a.delivery_status(id) != Some(DeliveryStatus::Delivered) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    // Publishing the signed preference makes updated senders reject before enqueue.
    timeout(
        Duration::from_secs(8),
        b.set_dm_language_policy(Some("en".into())),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(a.dm_language_policy(&b.account_id()).as_deref(), Some("en"));
    assert!(
        a.enqueue_to_account(&b.account_id(), original.as_bytes(), None)
            .await
            .is_err()
    );
    let sender = a.account_id();
    at.abort();
    bt.abort();
    let _ = at.await;
    let _ = bt.await;
    drop(b);
    let (reopened, _, _) = test_support::node_with_files(
        bd.path(),
        DeviceIdentity::from_secret_bytes(bob_secret.0, bob_secret.1),
        Account::from_secret_bytes(bob_account_secret),
        &ap,
    );
    assert_eq!(
        reopened
            .dm_language_policy(&reopened.account_id())
            .as_deref(),
        Some("en")
    );
    reopened.prepare_own_profile(None, Some(None)).unwrap();
    assert_eq!(
        reopened.account_history(&sender, 10)[0].text,
        language_io::HIDDEN_TEXT
    );
    assert!(reopened.search("午饭").is_empty());
}

#[test]
fn language_backup_preserves_preference_and_hidden_decisions() {
    let source = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let peer = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&peer, &Account::generate(), "peer", 9);
    let (node, _, _) = test_support::node_with_files(
        source.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    node.prepare_own_profile(None, Some(Some("zh".into())))
        .unwrap();
    let hidden = crate::eventlog::EventId::new([7; 32]);
    node.language_state
        .lock()
        .unwrap()
        .decide(hidden, true)
        .unwrap();
    let mut snapshot = Vec::new();
    node.write_recovery_snapshot(&mut snapshot, 100).unwrap();
    let restored = Node::install_recovery_snapshot(
        std::io::Cursor::new(snapshot),
        &destination.path().join("restored"),
        "pw",
    )
    .unwrap();
    assert_eq!(
        restored
            .dm_language_policy(&restored.account_id())
            .as_deref(),
        Some("zh")
    );
    assert!(restored.hidden_by_language(hidden));
}

#[tokio::test]
async fn language_reused_logical_id_cannot_expose_changed_plaintext() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
    let bp = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
    let (a, _, _) = test_support::node_with_files(ad.path(), alice, aa, &bp);
    let (b, mut messages, _) = test_support::node_with_files(bd.path(), bob, bob_account, &ap);
    b.prepare_own_profile(None, Some(Some("en".into())))
        .unwrap();
    let conversation = conversation::dm_conversation_id(&a.identity.public(), &b.identity.public());
    for (text, hidden) in [
        (
            "Please send the document before our meeting tomorrow.",
            false,
        ),
        ("你好，我们明天一起出去吃午饭吧。", true),
    ] {
        // An adversarial client chooses the same logical ID for DIFFERENT signed events.
        let plaintext = DmEnvelope::new(
            a.account_id(),
            b.account_id(),
            [3; 32],
            MessageBody::new(text.as_bytes().to_vec(), None).encode(),
        )
        .encode();
        let ciphertext = a
            .dm_ratchet
            .lock()
            .unwrap()
            .encrypt(&a.identity, &b.identity.public(), &plaintext)
            .unwrap();
        let seq = a
            .append_event(
                conversation,
                crate::eventlog::EventKind::Message,
                ciphertext,
            )
            .unwrap();
        let event = a
            .log
            .lock()
            .unwrap()
            .events(&conversation)
            .into_iter()
            .find(|event| event.seq == seq)
            .unwrap()
            .clone();
        assert!(
            b.accept_dm_event(&event, &ap, &mut b.delivery.lock().unwrap())
                .is_some()
        );
        let message = messages.try_recv().unwrap();
        assert_eq!(
            message.text,
            if hidden {
                language_io::HIDDEN_TEXT.to_vec()
            } else {
                text.as_bytes().to_vec()
            }
        );
    }
}

#[test]
fn language_backfill_import_is_filtered_before_projection() {
    let dir = tempfile::tempdir().unwrap();
    let sender = DeviceIdentity::generate();
    let account = Account::generate();
    let proof = Announce::new_with_account(&sender, &account, "peer", 9);
    let (node, _, _) = test_support::node_with_files(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    node.prepare_own_profile(None, Some(Some("en".into())))
        .unwrap();
    let id = [9; 32];
    let env = DmEnvelope::new(
        account.account_id(),
        node.account_id(),
        id,
        MessageBody::new("你好，我们明天一起出去吃午饭吧。".as_bytes().to_vec(), None).encode(),
    );
    node.import_account_backfill(&[BackfillRecord {
        conv: *conversation::account_conversation_id(&account.account_id(), &node.account_id())
            .as_bytes(),
        from: account.account_id(),
        wall_clock: 100,
        plaintext: env.encode(),
        event_id: id,
    }])
    .unwrap();
    assert_eq!(
        node.account_history(&account.account_id(), 10)[0].text,
        language_io::HIDDEN_TEXT
    );
    assert!(node.search("午饭").is_empty());
    assert!(node.export_account_backfill().is_empty());
}

#[tokio::test]
async fn language_final_acceptance_rechecks_new_recipient_preference() {
    let dir = tempfile::tempdir().unwrap();
    let peer = DeviceIdentity::generate();
    let account = Account::generate();
    let proof = Announce::new_with_account(&peer, &account, "peer", 9);
    let (node, _, _) = test_support::node_with_files(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let policy = profile::ProfilePayload::new_with_language(&account, None, 200, Some("en".into()));
    let mut calls = 0;
    let result = node
        .enqueue_to_account_if(
            &account.account_id(),
            "你好，我们明天一起出去吃午饭吧。".as_bytes(),
            None,
            |accept| {
                calls += 1;
                if calls >= 2 {
                    node.language_state
                        .lock()
                        .unwrap()
                        .record_profile(&policy)
                        .unwrap();
                }
                accept()
            },
        )
        .await;
    assert!(result.is_err());
    assert!(calls >= 2);
    assert!(node.account_history(&account.account_id(), 10).is_empty());
}

#[tokio::test]
async fn language_legacy_sibling_avatar_keeps_modern_preference() {
    let dir = tempfile::tempdir().unwrap();
    let sibling = DeviceIdentity::generate();
    let account = Account::generate();
    let own = Account::from_secret_bytes(account.secret_bytes());
    let proof = Announce::new_with_account(&sibling, &account, "sibling", 9);
    let (node, _, _) =
        test_support::node_with_files(dir.path(), DeviceIdentity::generate(), own, &proof);
    let current = node
        .prepare_own_profile(None, Some(Some("en".into())))
        .unwrap();
    let legacy = profile::ProfilePayload::new(
        &account,
        Some(b"new avatar".to_vec()),
        current.updated_at + 10,
    );
    let conversation = conversation::dm_conversation_id(&sibling.public(), &node.identity.public());
    let ciphertext = crate::dm::seal(
        &sibling,
        &node.identity.public().x25519_pub,
        &legacy.encode(),
    )
    .unwrap();
    let event = crate::eventlog::Event::new(
        &sibling,
        conversation,
        1,
        Vec::new(),
        1,
        legacy.updated_at,
        crate::eventlog::EventKind::Profile,
        ciphertext,
    );
    node.log.lock().unwrap().append(event).unwrap();
    node.process_profile_events(conversation);
    let cached = node.my_profile.lock().unwrap().clone().unwrap();
    assert!(cached.language_supported);
    assert_eq!(cached.language.as_deref(), Some("en"));
    assert_eq!(cached.avatar, legacy.avatar);
    assert_eq!(
        node.language_state
            .lock()
            .unwrap()
            .profile(&node.account_id())
            .unwrap()
            .updated_at,
        cached.updated_at,
        "republished and effective policy revisions must agree"
    );
    assert!(cached.verify());
    node.set_avatar(Some(b"another avatar".to_vec()))
        .await
        .unwrap();
    assert_eq!(
        node.dm_language_policy(&node.account_id()).as_deref(),
        Some("en")
    );
}
