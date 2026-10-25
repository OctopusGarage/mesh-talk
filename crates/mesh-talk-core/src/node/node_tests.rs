use super::*;
use crate::channel::ChannelMeta;
use crate::discovery::announce::Announce;
use crate::node::conversation::dm_conversation_id;
use crate::node::message::MessageBody;
use crate::node::postbox::run_relay_accept_loop;
use crate::node::{MessageDestination, OutgoingMessageRequest, SyncCategory, SyncPhase};
use crate::postoffice::PostOffice;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::TcpListener;

#[tokio::test]
async fn removing_account_removes_linked_devices_in_one_epoch_and_rechecks_stale_enrollment() {
    use crate::identity::account::Account;
    let directory = tempfile::tempdir().unwrap();
    let owner_identity = DeviceIdentity::generate();
    let owner_account = Account::generate();
    let owner_account_id = owner_account.account_id();
    let account = Account::generate();
    let devices = [
        DeviceIdentity::generate(),
        DeviceIdentity::generate(),
        DeviceIdentity::generate(),
    ];
    let roster = Arc::new(Mutex::new(Roster::default()));
    let (dm, _) = mpsc::unbounded_channel();
    let (channel_tx, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    let owner = Node::open_with_account(
        owner_identity,
        owner_account,
        Arc::clone(&roster),
        dm,
        channel_tx,
        file,
        &directory.path().join("node.log"),
        &directory.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    for (index, device) in devices.iter().enumerate() {
        let proof = Announce::new_with_account(
            device,
            &account,
            format!("device-{index}"),
            47001 + index as u16,
        );
        roster
            .lock()
            .unwrap()
            .update(&proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &owner.user_id());
    }
    let channel = owner
        .create_channel("two people", vec![devices[0].public(), devices[1].public()])
        .await
        .unwrap();
    let stale = owner
        .auto_enrollment_candidate()
        .expect("third certified sibling is eligible");
    let before = owner
        .channels
        .lock()
        .unwrap()
        .state(&channel)
        .unwrap()
        .epoch();
    assert!(
        owner
            .remove_channel_account_members_if(channel, &owner_account_id, |commit| commit())
            .await
            .is_err()
    );
    assert!(
        owner
            .remove_channel_account_members_if(channel, &account.account_id(), |_commit| Err(
                NodeError::Authorization("retired owner".into())
            ))
            .await
            .is_err()
    );
    assert_eq!(owner.channel_members(channel).len(), 3);
    owner
        .remove_channel_account_members_if(channel, &account.account_id(), |commit| commit())
        .await
        .unwrap();
    assert_eq!(
        owner.channel_members(channel),
        vec![owner.identity.public()]
    );
    assert_eq!(
        owner
            .channels
            .lock()
            .unwrap()
            .state(&channel)
            .unwrap()
            .epoch(),
        before + 1
    );
    assert!(
        !owner
            .add_channel_member_checked_if(stale.0, stale.1, true, |commit| commit())
            .await
            .unwrap()
    );
    assert!(owner.auto_enrollment_candidate().is_none());
    assert_eq!(
        owner
            .channels
            .lock()
            .unwrap()
            .state(&channel)
            .unwrap()
            .epoch(),
        before + 1
    );
    owner
        .remove_channel_account_members_if(channel, &account.account_id(), |commit| commit())
        .await
        .unwrap();
    assert_eq!(
        owner
            .channels
            .lock()
            .unwrap()
            .state(&channel)
            .unwrap()
            .epoch(),
        before + 1
    );
}

#[tokio::test]
async fn removing_account_uses_durable_bindings_and_controls_when_projection_is_deferred() {
    use crate::identity::account::Account;
    let directory = tempfile::tempdir().unwrap();
    let owner_identity = DeviceIdentity::generate();
    let owner_account = Account::generate();
    let account = Account::generate();
    let devices = [
        DeviceIdentity::generate(),
        DeviceIdentity::generate(),
        DeviceIdentity::generate(),
    ];
    let roster = Arc::new(Mutex::new(Roster::default()));
    let (dm, _) = mpsc::unbounded_channel();
    let (channel_tx, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    let owner = Node::open_with_account(
        owner_identity,
        owner_account,
        Arc::clone(&roster),
        dm,
        channel_tx,
        file,
        &directory.path().join("node.log"),
        &directory.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    for (index, device) in devices.iter().enumerate() {
        let proof = Announce::new_with_account(
            device,
            &account,
            format!("device-{index}"),
            47001 + index as u16,
        );
        roster
            .lock()
            .unwrap()
            .update(&proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &owner.user_id());
    }
    let channel = owner
        .create_channel("two people", vec![devices[0].public(), devices[1].public()])
        .await
        .unwrap();
    let stale = owner
        .auto_enrollment_candidate()
        .expect("third certified sibling is eligible");
    let own = Announce::new_with_account(&owner.identity, &owner.account, "owner", 47000);
    owner
        .configure_privacy(
            directory.path(),
            "pw",
            &own,
            Arc::new(crate::discovery::DiscoveryVisibility::default()),
        )
        .unwrap();
    owner.cached_peer_snapshot().unwrap();
    owner.set_invisible(true).await.unwrap();
    owner
        .set_allowed(&account.account_id(), false)
        .await
        .unwrap();
    let plaintext_retry = Some(std::time::Instant::now() + Duration::from_secs(30));
    owner.pending_channel_plaintext.lock().unwrap().retry_at = plaintext_retry;
    owner
        .remove_channel_account_members_if(channel, &account.account_id(), |commit| commit())
        .await
        .unwrap();
    // Canonical membership refresh is independent of the paused plaintext
    // retry; removed accounts must disappear without waiting for body writes.
    assert_eq!(
        owner.channel_members(channel),
        vec![owner.identity.public()]
    );
    assert_eq!(
        owner.pending_channel_plaintext.lock().unwrap().retry_at,
        plaintext_retry
    );
    {
        let log = owner.log.lock().unwrap();
        let events = log.events(&channel);
        let durable = ChannelBook::projected_members(channel, &events).unwrap();
        assert_eq!(durable, vec![owner.identity.public()]);
    }
    assert!(
        !owner
            .add_channel_member_checked_if(stale.0, stale.1, true, |commit| commit())
            .await
            .unwrap()
    );
    // Repeating removal reads the durable epoch and is a true no-op.
    owner
        .remove_channel_account_members_if(channel, &account.account_id(), |commit| commit())
        .await
        .unwrap();
    owner.pending_channel_plaintext.lock().unwrap().retry_at = None;
    owner.process_channel(channel);
    assert_eq!(
        owner.channel_members(channel),
        vec![owner.identity.public()]
    );
    assert!(owner.auto_enrollment_candidate().is_none());
}

#[tokio::test]
async fn auto_enrollment_candidate_requires_accepted_certified_account_sibling() {
    use crate::identity::account::Account;

    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let alice_account = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_sibling = DeviceIdentity::generate();
    let bob_relay = DeviceIdentity::generate();
    let outsider = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let outsider_account = Account::generate();
    let roster = Arc::new(Mutex::new(Roster::default()));
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    let owner = Node::open_with_account(
        alice,
        alice_account,
        Arc::clone(&roster),
        dm,
        channel,
        file,
        &dir.path().join("node.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let self_id = owner.user_id();
    for announce in [
        Announce::new_with_account(&bob, &bob_account, "Bob", 47001),
        Announce::new_with_account(&bob_sibling, &bob_account, "Bob laptop", 47002),
        Announce::new_post_office_with_account(&bob_relay, &bob_account, "Relay", 47003),
        Announce::new_with_account(&outsider, &outsider_account, "Mallory", 47004),
    ] {
        roster
            .lock()
            .unwrap()
            .update(&announce, IpAddr::V4(Ipv4Addr::LOCALHOST), &self_id);
    }
    let invited = owner
        .create_channel_invited("pending", vec![bob.public()])
        .await
        .unwrap();
    assert!(owner.auto_enrollment_candidate().is_none());

    let accepted = owner
        .create_channel("accepted", vec![bob.public()])
        .await
        .unwrap();
    assert_eq!(
        owner.auto_enrollment_candidate(),
        Some((accepted, bob_sibling.public()))
    );
    assert_ne!(invited, accepted);

    // Retaining the same physical owner key does not authorize its legacy
    // groups for a newly adopted account. Only explicit source provenance does.
    *owner.channel_migration.lock().unwrap() =
        super::super::channel_account_maintenance::ChannelMigrationStore::open(
            &dir.path().join("unknown-origin.log"),
            "pw",
            owner.account.public().ed25519_pub,
            &owner.identity.public(),
        )
        .unwrap();
    assert!(owner.auto_enrollment_candidate().is_none());
    owner
        .authorize_legacy_channel_migration_if(|commit| commit())
        .unwrap();
    assert_eq!(
        owner.auto_enrollment_candidate(),
        Some((accepted, bob_sibling.public()))
    );
}

#[tokio::test]
async fn auto_enrolls_certified_sibling_with_owner_signed_invitation() {
    use crate::identity::account::Account;

    let dir = tempfile::tempdir().unwrap();
    let alice_account = Account::generate();
    let bob_account = Account::generate();
    let bob_secret = bob_account.secret_bytes();
    let open = |name: &str, identity: DeviceIdentity, account: Account| {
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, _) = mpsc::unbounded_channel();
        let (file, _) = mpsc::unbounded_channel();
        Node::open_with_account(
            identity,
            account,
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            &dir.path().join(format!("{name}-node.log")),
            &dir.path().join(format!("{name}-sent.log")),
            "pw",
        )
        .unwrap()
    };
    let alice = open("alice", DeviceIdentity::generate(), alice_account);
    let bob = open("bob", DeviceIdentity::generate(), bob_account);
    let sibling = open(
        "sibling",
        DeviceIdentity::generate(),
        Account::from_secret_bytes(bob_secret),
    );
    let bob_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let sibling_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let bob_port = bob_listener.local_addr().unwrap().port();
    let sibling_port = sibling_listener.local_addr().unwrap().port();
    let bob_task = tokio::spawn(Arc::clone(&bob).run_accept_loop(bob_listener));
    let sibling_task = tokio::spawn(Arc::clone(&sibling).run_accept_loop(sibling_listener));
    let announce = |target: &Arc<Node>, peer: &Arc<Node>, name: &str, port| {
        target.roster.lock().unwrap().update(
            &Announce::new_with_account(&peer.identity, &peer.account, name, port),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &target.user_id(),
        );
    };
    announce(&alice, &bob, "Bob", bob_port);
    announce(&alice, &sibling, "Bob laptop", sibling_port);
    announce(&bob, &alice, "Alice", 47005);
    announce(&sibling, &alice, "Alice", 47005);
    announce(&sibling, &bob, "Bob", bob_port);

    let channel = alice
        .create_channel("team", vec![bob.identity.public()])
        .await
        .unwrap();
    assert!(alice.auto_enroll_one_channel_sibling().await.unwrap());
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if sibling
                .list_channels()
                .iter()
                .any(|item| item.id == channel && !item.pending_invite)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("sibling automatically accepts the owner-signed invitation");
    // The accept-loop worker may consume the invitation before this task ever
    // observes its pending state. Assert its stable, signed acceptance instead.
    assert_eq!(sibling.channel_owner(channel), alice.user_id());
    let acceptance = sibling
        .channels
        .lock()
        .unwrap()
        .state(&channel)
        .unwrap()
        .accepted_event_id()
        .expect("automatic acceptance leaves a signed event");
    {
        let log = sibling.log.lock().unwrap();
        let event = log.get(&acceptance).unwrap();
        assert_eq!(event.kind, EventKind::ChannelAccept);
        assert_eq!(event.author.user_id(), sibling.user_id());
    }
    assert!(!sibling.auto_accept_one_linked_invite().await.unwrap());
    let ordinary = alice
        .create_channel_invited("ordinary", vec![sibling.identity.public()])
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if sibling
                .list_channels()
                .iter()
                .any(|item| item.id == ordinary && item.pending_invite)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("ordinary invitation reaches sibling device");
    assert_eq!(sibling.linked_invite_candidate(), None);
    assert!(!sibling.auto_accept_one_linked_invite().await.unwrap());
    let epoch = alice
        .channels
        .lock()
        .unwrap()
        .state(&channel)
        .unwrap()
        .epoch();
    assert!(!alice.auto_enroll_one_channel_sibling().await.unwrap());
    assert_eq!(
        alice
            .channels
            .lock()
            .unwrap()
            .state(&channel)
            .unwrap()
            .epoch(),
        epoch
    );
    sibling_task.abort();
    bob_task.abort();
}

#[test]
fn post_office_use_can_be_disabled_and_reenabled() {
    let dir = tempfile::tempdir().unwrap();
    let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
    let (channel_tx, _channel_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let node = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm_tx,
        channel_tx,
        file_tx,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let relay = DeviceIdentity::generate().public();

    assert!(node.relay_allowed(&relay));
    node.set_post_office_enabled(false);
    assert!(!node.relay_allowed(&relay));
    node.set_post_office_enabled(true);
    assert!(node.relay_allowed(&relay));
}

#[test]
fn disabling_post_office_use_clears_bridge_introductions() {
    let dir = tempfile::tempdir().unwrap();
    let roster = Arc::new(Mutex::new(Roster::default()));
    let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
    let (channel_tx, _channel_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let node = Node::open(
        DeviceIdentity::generate(),
        Arc::clone(&roster),
        dm_tx,
        channel_tx,
        file_tx,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let remote = DeviceIdentity::generate();
    let proof = Announce::new_with_account(
        &remote,
        &crate::identity::account::Account::generate(),
        "Remote",
        47475,
    );
    node.set_bridge_discovery_enabled(true);
    roster.lock().unwrap().update_relayed(
        &proof,
        "127.0.0.1:47500".parse().unwrap(),
        &node.user_id(),
    );
    node.bridge_dm_pull_peers
        .lock()
        .unwrap()
        .insert(proof.user_id.clone(), std::time::Instant::now());
    assert!(node.bridge_directory_allowed());
    assert!(node.verified_dm_pull_peer(&proof.user_id));

    node.set_post_office_enabled(false);

    assert!(!node.bridge_directory_allowed());
    assert!(!node.verified_dm_pull_peer(&proof.user_id));
    assert!(roster.lock().unwrap().get(&proof.user_id).is_none());
    assert!(node.bridge_dm_pull_peers.lock().unwrap().is_empty());
}

#[tokio::test]
async fn invisible_mode_clears_bridge_presence_and_recovery_permission() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = crate::identity::account::Account::generate();
    let own = Announce::new_with_account(&identity, &account, "Me", 47475);
    let roster = Arc::new(Mutex::new(Roster::default()));
    let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
    let (channel_tx, _channel_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let node = Node::open_with_account(
        identity,
        account,
        Arc::clone(&roster),
        dm_tx,
        channel_tx,
        file_tx,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    node.configure_privacy(
        dir.path(),
        "pw",
        &own,
        Arc::new(crate::discovery::DiscoveryVisibility::default()),
    )
    .unwrap();
    let remote = DeviceIdentity::generate();
    let proof = Announce::new_with_account(
        &remote,
        &crate::identity::account::Account::generate(),
        "Remote",
        47475,
    );
    node.set_bridge_discovery_enabled(true);
    roster.lock().unwrap().update_relayed(
        &proof,
        "127.0.0.1:47500".parse().unwrap(),
        &node.user_id(),
    );
    node.bridge_dm_pull_peers
        .lock()
        .unwrap()
        .insert(proof.user_id.clone(), std::time::Instant::now());
    assert!(node.verified_dm_pull_peer(&proof.user_id));
    node.set_invisible(true).await.unwrap();
    assert!(!node.verified_dm_pull_peer(&proof.user_id));
    assert!(roster.lock().unwrap().get(&proof.user_id).is_none());
}

#[tokio::test]
async fn channel_file_card_carries_fixed_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let (dm, _) = mpsc::unbounded_channel();
    let (channel_rx, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    let node = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel_rx,
        file,
        &dir.path().join("node.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let channel = node.create_channel("ttl", vec![]).await.unwrap();
    let path = dir.path().join("secret.txt");
    std::fs::write(&path, b"secret").unwrap();
    let deadline = super::now_millis() + 300_000;
    node.send_file_channel_progress_expiring(
        channel,
        &path,
        crate::file::FileKind::File,
        |_| {},
        Some(deadline),
    )
    .await
    .unwrap();
    let rows = node.received_files.lock().unwrap().entries(&channel);
    assert_eq!(
        crate::expiry::deadline(&rows.last().unwrap().plaintext),
        Some(deadline)
    );
}

#[tokio::test]
async fn channel_message_carries_fixed_deadline_in_sender_history() {
    let dir = tempfile::tempdir().unwrap();
    let (dm, _) = mpsc::unbounded_channel();
    let (channel_rx, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    let node = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel_rx,
        file,
        &dir.path().join("node.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let channel = node.create_channel("ttl", vec![]).await.unwrap();
    let deadline = super::now_millis() + 300_000;
    node.send_channel_message_reply_expiring(channel, b"hello", None, Some(deadline))
        .await
        .unwrap();
    let sent = node.sentlog.lock().unwrap().entries(&channel);
    assert_eq!(
        crate::expiry::deadline(&sent.last().unwrap().plaintext),
        Some(deadline)
    );
}

#[tokio::test]
async fn expired_outgoing_account_message_is_hidden_and_erased() {
    let dir = tempfile::tempdir().unwrap();
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    let mut node = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        file,
        &dir.path().join("node.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let remote = crate::identity::account::Account::generate();
    let peer = remote.account_id();
    let proof = crate::discovery::Announce::new_with_account(
        &DeviceIdentity::generate(),
        &remote,
        "Bob",
        9,
    );
    node.roster
        .lock()
        .unwrap()
        .update(&proof, "127.0.0.1".parse().unwrap(), &node.user_id());
    let deadline = super::now_millis() + 300_000;
    let id = node
        .enqueue_to_account_expiring_if(
            &peer,
            b"ephemeral secret",
            None,
            Some(deadline),
            |accept| accept(),
        )
        .await
        .unwrap();
    assert_eq!(
        node.account_history(&peer, 20)[0].expires_at_ms,
        Some(deadline)
    );
    let backfill = node.export_account_backfill();
    assert_eq!(backfill.len(), 1);
    Arc::get_mut(&mut node).unwrap().expiry_clock_start = (std::time::Instant::now(), deadline + 1);
    assert!(
        node.account_history(&peer, 20).is_empty(),
        "sender history must expire without waiting for maintenance"
    );
    assert!(node.search("ephemeral secret").is_empty());
    assert!(node.export_account_backfill().is_empty());
    node.import_account_backfill(&backfill).unwrap();
    let conv = crate::node::conversation::account_conversation_id(&node.account_id(), &peer);
    assert!(node.received.lock().unwrap().entries(&conv).is_empty());
    assert!(node.prune_expired_received().unwrap() > 0);
    let conv = crate::node::conversation::account_conversation_id(&node.account_id(), &peer);
    assert!(
        node.sent_entries_for_history(conv).is_empty(),
        "maintenance must erase sender plaintext including pending delivery"
    );
    assert!(node.account_suppression.lock().unwrap().contains(conv, id));
    let secret = node.identity.secret_bytes();
    drop(node);
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    let reopened = Node::open(
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        file,
        &dir.path().join("node.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    assert!(reopened.account_history(&peer, 20).is_empty());
    assert!(reopened.sent_entries_for_history(conv).is_empty());
}

#[test]
fn expired_incoming_account_message_is_hidden_from_history() {
    use crate::eventlog::EventId;
    use crate::node::conversation::account_conversation_id;
    let dir = tempfile::tempdir().unwrap();
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    let node = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        file,
        &dir.path().join("node.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let peer = crate::identity::account::Account::generate().account_id();
    let own = node.account_id();
    let conv = account_conversation_id(&own, &peer);
    let body = MessageBody::new(b"expired secret".to_vec(), None).encode();
    let deadline = super::now_millis().saturating_sub(1);
    let env = crate::node::DmEnvelope::new(
        peer.clone(),
        own,
        [7; 32],
        crate::expiry::wrap(&body, Some(deadline)),
    );
    node.received
        .lock()
        .unwrap()
        .record(
            conv,
            peer.clone(),
            deadline.saturating_sub(1),
            &env.encode(),
            EventId::new([8; 32]),
        )
        .unwrap();
    assert!(node.account_history(&peer, 20).is_empty());
    assert_eq!(node.prune_expired_received().unwrap(), 1);
    assert!(node.received.lock().unwrap().entries(&conv).is_empty());
}

#[test]
fn restart_erases_unexpired_recipient_copy_without_erasing_sender_copy() {
    use crate::node::conversation::account_conversation_id;
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let secret = identity.secret_bytes();
    let open = || {
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, _) = mpsc::unbounded_channel();
        let (file, _) = mpsc::unbounded_channel();
        Node::open(
            DeviceIdentity::from_secret_bytes(secret.0, secret.1),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            &dir.path().join("node.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap()
    };
    let node = open();
    let peer = crate::identity::account::Account::generate().account_id();
    let own = node.account_id();
    let conv = account_conversation_id(&own, &peer);
    let deadline = super::now_millis() + 3_600_000;
    for (from, id) in [(peer.clone(), [13; 32]), (own.clone(), [14; 32])] {
        let body = crate::expiry::wrap(
            &MessageBody::new(b"secret".to_vec(), None).encode(),
            Some(deadline),
        );
        let env = crate::node::DmEnvelope::new(
            from.clone(),
            if from == own {
                peer.clone()
            } else {
                own.clone()
            },
            id,
            body,
        );
        node.received
            .lock()
            .unwrap()
            .record(
                conv,
                from,
                super::now_millis(),
                &env.encode(),
                crate::eventlog::EventId::new(id),
            )
            .unwrap();
    }
    drop(node);
    let reopened = open();
    let rows = reopened.received.lock().unwrap().entries(&conv);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].from, own);
    assert!(
        reopened
            .account_suppression
            .lock()
            .unwrap()
            .contains(conv, crate::eventlog::EventId::new([13; 32]))
    );
}

#[test]
fn monotonic_observation_bounds_expiry_even_with_a_future_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    let node = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        file,
        &dir.path().join("node.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let id = crate::eventlog::EventId::new([91; 32]);
    let deadline = super::now_millis() + 3_600_000;
    let wire = crate::expiry::wrap(b"secret", Some(deadline));
    assert!(!node.recipient_expired(&wire, id, deadline - 10));
    node.expiry_first_seen.lock().unwrap().insert(
        id,
        std::time::Instant::now() - std::time::Duration::from_millis(20),
    );
    assert!(node.recipient_expired(&wire, id, deadline - 10));
}

#[test]
fn recovery_snapshot_carries_history_without_reusing_device_identity() {
    use crate::eventlog::event::{ConversationId, EventId};
    use crate::recovery::snapshot::{SnapshotRecordV1, read_snapshot};
    use std::io::Cursor;

    let dir = tempfile::tempdir().unwrap();
    let me = DeviceIdentity::generate();
    let old_device_id = me.public().user_id();
    let account_id =
        crate::identity::account::Account::from_secret_bytes(me.secret_bytes().0).account_id();
    let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
    let (channel_tx, _channel_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let node = Node::open(
        me,
        Arc::new(Mutex::new(Roster::default())),
        dm_tx,
        channel_tx,
        file_tx,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "password",
    )
    .unwrap();
    let conversation = ConversationId::new([3; 32]);
    node.sentlog
        .lock()
        .unwrap()
        .record(conversation, 1, 11, b"sent")
        .unwrap();
    node.received
        .lock()
        .unwrap()
        .record(
            conversation,
            "peer".into(),
            12,
            b"received",
            EventId::new([4; 32]),
        )
        .unwrap();
    let media_id = ConversationId::new([9; 32]);
    let media_bytes = vec![0x42; 70_000];
    node.media
        .store_bytes(media_id, "photo.png", &media_bytes)
        .unwrap();
    // Locally retained media without a visible file card must stay out of backup.
    node.media
        .store_bytes(
            ConversationId::new([10; 32]),
            "orphan.png",
            b"deleted bytes",
        )
        .unwrap();
    node.recalls
        .lock()
        .unwrap()
        .record(conversation, EventId::new([5; 32]), 15)
        .unwrap();
    node.account_profiles
        .restore_avatar("peer", 16, Some(b"avatar".to_vec()))
        .unwrap();
    let signed = crate::eventlog::event::Event::new(
        &node.identity,
        ConversationId::new([6; 32]),
        1,
        vec![],
        1,
        17,
        crate::eventlog::event::EventKind::Message,
        b"sealed".to_vec(),
    );
    node.log.lock().unwrap().append(signed.clone()).unwrap();
    let file_manifest = crate::file::FileManifest {
        name: "photo.png".into(),
        size: media_bytes.len() as u64,
        mime: "image/png".into(),
        checksum: crate::file::file_checksum(&media_bytes),
        file_key: [3; 32],
        file_conv: media_id,
        chunk_count: crate::file::chunk_count_for(media_bytes.len() as u64),
    };
    let file_event = crate::eventlog::event::Event::new(
        &node.identity,
        conversation,
        2,
        vec![],
        2,
        18,
        crate::eventlog::event::EventKind::FileManifest,
        b"sealed file card".to_vec(),
    );
    node.log.lock().unwrap().append(file_event.clone()).unwrap();
    node.received_files
        .lock()
        .unwrap()
        .record(
            conversation,
            node.user_id(),
            18,
            &file_manifest.encode(),
            file_event.id,
        )
        .unwrap();

    let mut snapshot = Vec::new();
    node.write_recovery_snapshot(&mut snapshot, 100).unwrap();
    let mut records = Vec::new();
    let header = read_snapshot(Cursor::new(snapshot.as_slice()), |record| {
        records.push(record);
        Ok(())
    })
    .unwrap();
    assert_eq!(header.account_id, account_id);
    assert_eq!(records.len(), 11);
    assert!(records.iter().any(|record| matches!(record,
        SnapshotRecordV1::AccountCreation { created_at_ms } if *created_at_ms == node.account.created_at_ms()
    )));
    assert!(records.iter().any(|record| matches!(record, SnapshotRecordV1::SentDm { plaintext, .. } if plaintext == b"sent")));
    assert!(records.iter().any(|record| matches!(record, SnapshotRecordV1::ReceivedDm { plaintext, .. } if plaintext == b"received")));
    assert!(records.iter().any(|record| matches!(record, SnapshotRecordV1::MediaDescriptor { file_conv, size, .. } if *file_conv == [9; 32] && *size == media_bytes.len() as u64)));
    let recovered_media: Vec<u8> = records
        .iter()
        .filter_map(|record| match record {
            SnapshotRecordV1::MediaChunk {
                file_conv, bytes, ..
            } if *file_conv == [9; 32] => Some(bytes.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect();
    assert_eq!(recovered_media, media_bytes);
    assert!(records.iter().any(
        |record| matches!(record, SnapshotRecordV1::Recall { target, .. } if *target == [5; 32])
    ));
    assert!(records.iter().any(|record| matches!(record, SnapshotRecordV1::Profile { account_id, avatar, .. } if account_id == "peer" && avatar.as_deref() == Some(b"avatar"))));
    assert!(records.iter().any(
        |record| matches!(record, SnapshotRecordV1::SignedEvent { event } if event.id == signed.id)
    ));
    assert!(records.iter().any(|record| matches!(record, SnapshotRecordV1::FileCard { event_id, .. } if *event_id == *file_event.id.as_bytes())));

    let key = crate::recovery::key::RecoveryKey::generate();
    let mut archive = Vec::new();
    node.write_encrypted_recovery_archive(&mut archive, &key, 101)
        .unwrap();
    let preview =
        Node::inspect_encrypted_recovery_archive(Cursor::new(archive), &key, dir.path()).unwrap();
    assert_eq!(preview.messages, 2);
    assert_eq!(preview.channels, 0);
    assert_eq!(preview.file_cards, 1);
    assert_eq!(preview.media_files, 1);
    assert_eq!(preview.missing_media, 0);

    let restored = Node::install_recovery_snapshot(
        Cursor::new(snapshot),
        &dir.path().join("restored"),
        "new password",
    )
    .unwrap();
    assert_eq!(restored.account_id(), account_id);
    assert_eq!(
        restored.account.created_at_ms(),
        node.account.created_at_ms()
    );
    assert_ne!(restored.user_id(), old_device_id);
    assert_eq!(restored.history(conversation, 10).len(), 3);
    assert!(
        restored
            .history(conversation, 10)
            .iter()
            .find(|entry| entry.id == file_event.id)
            .unwrap()
            .from_me,
        "a file sent from the backed-up device remains mine after restore"
    );
    assert_eq!(restored.peer_avatar("peer"), Some(b"avatar".to_vec()));
    let mut restored_media = Vec::new();
    restored
        .media
        .copy_decrypted(media_id, &mut restored_media)
        .unwrap();
    assert_eq!(restored_media.len(), media_bytes.len());
    assert!(restored_media == media_bytes);
    assert!(restored.media.read(media_id).is_some());
    assert_eq!(
        restored.media.path(media_id).unwrap().extension().unwrap(),
        "png"
    );
    assert!(!restored.media.contains(ConversationId::new([10; 32])));
    let mut second_snapshot = Vec::new();
    restored
        .write_recovery_snapshot(&mut second_snapshot, 200)
        .unwrap();
    let twice_restored = Node::install_recovery_snapshot(
        Cursor::new(second_snapshot),
        &dir.path().join("twice-restored"),
        "third password",
    )
    .unwrap();
    assert!(
        twice_restored
            .history(conversation, 10)
            .iter()
            .find(|entry| entry.id == file_event.id)
            .unwrap()
            .from_me,
        "file ownership survives another backup and restore"
    );
    drop(restored);
    let restored_dir = dir.path().join("restored");
    assert!(
        crate::identity::keystore::load(&restored_dir.join("identity.keystore"), "old password")
            .is_err()
    );
    let identity =
        crate::identity::keystore::load(&restored_dir.join("identity.keystore"), "new password")
            .unwrap();
    let account = crate::identity::account_keystore::load(
        &restored_dir.join("account.keystore"),
        "new password",
    )
    .unwrap();
    let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
    let (channel_tx, _channel_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let reopened = Node::open_with_account(
        identity,
        account,
        Arc::new(Mutex::new(Roster::default())),
        dm_tx,
        channel_tx,
        file_tx,
        &restored_dir.join("messages.log"),
        &restored_dir.join("sent.log"),
        "new password",
    )
    .unwrap();
    assert_eq!(reopened.account_id(), account_id);
    assert_eq!(reopened.history(conversation, 10).len(), 3);
    assert!(reopened.log.lock().unwrap().has(&signed.id));
    assert!(reopened.log.lock().unwrap().has(&file_event.id));
    assert_eq!(
        reopened
            .received_files
            .lock()
            .unwrap()
            .entries(&conversation)
            .len(),
        1
    );
}

#[test]
fn account_history_keeps_a_contact_visible_after_discovery_expires() {
    use crate::eventlog::event::{ConversationId, EventId};
    use crate::node::DmEnvelope;
    let dir = tempfile::tempdir().unwrap();
    let me = DeviceIdentity::generate();
    let saved_device_secret = me.secret_bytes();
    let own =
        crate::identity::account::Account::from_secret_bytes(me.secret_bytes().0).account_id();
    let peer = crate::identity::account::Account::generate().account_id();
    let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
    let (channel_tx, _channel_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let node = Node::open(
        me,
        Arc::new(Mutex::new(Roster::default())),
        dm_tx,
        channel_tx,
        file_tx,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "password",
    )
    .unwrap();
    let envelope = DmEnvelope::new(peer.clone(), own, [7; 32], b"hello".to_vec());
    node.received
        .lock()
        .unwrap()
        .record(
            ConversationId::new([3; 32]),
            peer.clone(),
            1,
            &envelope.encode(),
            EventId::new([8; 32]),
        )
        .unwrap();
    assert_eq!(node.known_account_ids(), vec![peer.clone()]);
    drop(node);
    let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
    let (channel_tx, _channel_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let reopened = Node::open(
        DeviceIdentity::from_secret_bytes(saved_device_secret.0, saved_device_secret.1),
        Arc::new(Mutex::new(Roster::default())),
        dm_tx,
        channel_tx,
        file_tx,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "password",
    )
    .unwrap();
    assert_eq!(reopened.known_account_ids(), vec![peer]);
}

#[test]
fn recovery_rejects_a_snapshot_changed_between_validation_and_install() {
    use crate::recovery::snapshot::{SnapshotError, SnapshotHeaderV1, write_snapshot};
    use std::io::{Cursor, Read, Seek, SeekFrom};

    struct SwapOnRewind {
        first: Cursor<Vec<u8>>,
        second: Cursor<Vec<u8>>,
        switched: bool,
    }
    impl Read for SwapOnRewind {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if self.switched {
                self.second.read(bytes)
            } else {
                self.first.read(bytes)
            }
        }
    }
    impl Seek for SwapOnRewind {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            if position == SeekFrom::Start(0) {
                self.switched = true;
            }
            if self.switched {
                self.second.seek(position)
            } else {
                self.first.seek(position)
            }
        }
    }

    let snapshot_for = |secret: [u8; 32]| {
        let mut bytes = Vec::new();
        write_snapshot(
            &mut bytes,
            &SnapshotHeaderV1 {
                schema_version: 1,
                account_secret: secret,
                account_id: crate::identity::account::Account::from_secret_bytes(secret)
                    .account_id(),
                source_device_ed25519_pubs: vec![[1; 32]],
                created_at_ms: 1,
                record_count: 0,
            },
            [],
        )
        .unwrap();
        bytes
    };
    let source = SwapOnRewind {
        first: Cursor::new(snapshot_for([1; 32])),
        second: Cursor::new(snapshot_for([2; 32])),
        switched: false,
    };
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("restored");
    assert!(matches!(
        Node::install_recovery_snapshot(source, &destination, "new password"),
        Err(SnapshotError::InvalidAccount)
    ));
    assert!(!destination.exists());
}

#[tokio::test]
async fn recovered_channel_history_remains_readable_on_a_fresh_device() {
    use std::io::Cursor;
    let dir = tempfile::tempdir().unwrap();
    let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
    let (channel_tx, _channel_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let source = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm_tx,
        channel_tx,
        file_tx,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "old password",
    )
    .unwrap();
    let channel = source.create_channel("Archive", vec![]).await.unwrap();
    source
        .send_channel_message(channel, b"group history")
        .await
        .unwrap();
    let key = crate::recovery::key::RecoveryKey::generate();
    let mut archive = Vec::new();
    source
        .write_encrypted_recovery_archive(&mut archive, &key, 90)
        .unwrap();
    let preview =
        Node::inspect_encrypted_recovery_archive(Cursor::new(archive), &key, dir.path()).unwrap();
    assert_eq!(preview.channels, 1);
    assert_eq!(preview.messages, 1);
    let original_id = source.channel_history(channel, 10)[0].id;
    source.recall_channel(channel, original_id).await.unwrap();
    let member_dir = dir.path().join("member");
    std::fs::create_dir_all(&member_dir).unwrap();
    let (dm_tx, _member_dm_rx) = mpsc::unbounded_channel();
    let (channel_tx, _member_channel_rx) = mpsc::unbounded_channel();
    let (file_tx, _member_file_rx) = mpsc::unbounded_channel();
    let member = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm_tx,
        channel_tx,
        file_tx,
        &member_dir.join("messages.log"),
        &member_dir.join("sent.log"),
        "member password",
    )
    .unwrap();
    source
        .add_channel_member(channel, member.identity.public())
        .await
        .unwrap();
    assert_eq!(
        source.list_channels()[0].member_count,
        1,
        "pending invitees are not accepted members"
    );
    // Exercise the recipient's signed acceptance before snapshotting two members.
    // Copy the real event DAG without requiring a network listener in this recovery test.
    let invitation = source
        .log
        .lock()
        .unwrap()
        .events(&channel)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    for event in invitation {
        member.log.lock().unwrap().append(event).unwrap();
    }
    member.process_channel(channel);
    member.accept_channel_invite(channel).await.unwrap();
    let acceptance = member
        .log
        .lock()
        .unwrap()
        .events(&channel)
        .into_iter()
        .find(|event| event.kind == EventKind::ChannelAccept)
        .unwrap()
        .clone();
    source.log.lock().unwrap().append(acceptance).unwrap();
    source.process_channel(channel);
    assert_eq!(
        source.list_channels()[0].member_count,
        2,
        "snapshot includes both accepted members"
    );
    source
        .rename_channel(channel, "Renamed archive")
        .await
        .unwrap();
    source.dissolve_channel(channel).await.unwrap();
    let mut snapshot = Vec::new();
    source.write_recovery_snapshot(&mut snapshot, 100).unwrap();
    let restored_dir = dir.path().join("restored");
    let restored =
        Node::install_recovery_snapshot(Cursor::new(snapshot), &restored_dir, "new password")
            .unwrap();
    drop(restored);
    let identity =
        crate::identity::keystore::load(&restored_dir.join("identity.keystore"), "new password")
            .unwrap();
    let account = crate::identity::account_keystore::load(
        &restored_dir.join("account.keystore"),
        "new password",
    )
    .unwrap();
    let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
    let (channel_tx, _channel_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let reopened = Node::open_with_account(
        identity,
        account,
        Arc::new(Mutex::new(Roster::default())),
        dm_tx,
        channel_tx,
        file_tx,
        &restored_dir.join("messages.log"),
        &restored_dir.join("sent.log"),
        "new password",
    )
    .unwrap();
    let restored_channels = reopened.list_channels();
    let summary = restored_channels
        .iter()
        .find(|summary| summary.id == channel)
        .expect("restored group remains visible");
    assert!(summary.archived, "new device retains archive presentation");
    assert!(summary.dissolved, "restored archive retains dissolution");
    assert_eq!(
        summary.member_count, 2,
        "restored archive retains both member observations"
    );
    assert!(
        summary.name == "Renamed archive",
        "restored archive retains its last validated rename"
    );
    assert!(
        reopened
            .channel_history(channel, 10)
            .iter()
            .any(|entry| { entry.id == original_id && entry.recalled })
    );
    let mut second_snapshot = Vec::new();
    reopened
        .write_recovery_snapshot(&mut second_snapshot, 200)
        .unwrap();
    let twice_restored = Node::install_recovery_snapshot(
        Cursor::new(second_snapshot),
        &dir.path().join("twice-restored"),
        "third password",
    )
    .unwrap();
    assert!(
        twice_restored
            .channel_history(channel, 10)
            .iter()
            .any(|entry| entry.id == original_id && entry.recalled),
        "the original message ID and recall survive another backup and restore"
    );
    assert_eq!(
        reopened
            .delete_message(channel, original_id, false)
            .unwrap(),
        1
    );
    assert!(
        reopened
            .channel_history(channel, 10)
            .iter()
            .all(|entry| entry.id != original_id)
    );
}

#[tokio::test]
async fn recovered_channel_can_rejoin_send_and_reopen_without_sent_sequence_conflict() {
    use std::io::Cursor;
    let dir = tempfile::tempdir().unwrap();
    let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
    let (channel_tx, _channel_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let source = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm_tx,
        channel_tx,
        file_tx,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "old password",
    )
    .unwrap();
    let channel = source.create_channel("Archive", vec![]).await.unwrap();
    source
        .send_channel_message(channel, b"before recovery")
        .await
        .unwrap();
    let mut snapshot = Vec::new();
    source.write_recovery_snapshot(&mut snapshot, 100).unwrap();
    let restored_dir = dir.path().join("restored");
    let restored =
        Node::install_recovery_snapshot(Cursor::new(snapshot), &restored_dir, "new password")
            .unwrap();
    source
        .add_channel_member(channel, restored.identity.public())
        .await
        .unwrap();
    let invitation = (*source.log.lock().unwrap().events(&channel).last().unwrap()).clone();
    restored.log.lock().unwrap().append(invitation).unwrap();
    drop(restored);
    let rejoined = Node::open_recovery_profile(&restored_dir, "new password").unwrap();
    assert!(!rejoined.list_channels()[0].archived);
    assert!(rejoined.list_channels()[0].pending_invite);
    rejoined.accept_channel_invite(channel).await.unwrap();
    rejoined
        .send_channel_message(channel, b"after recovery one")
        .await
        .unwrap();
    rejoined
        .send_channel_message(channel, b"after recovery two")
        .await
        .unwrap();
    drop(rejoined);
    let reopened = Node::open_recovery_profile(&restored_dir, "new password").unwrap();
    assert_eq!(reopened.channel_history(channel, 10).len(), 3);
}

#[test]
fn file_only_account_contact_remains_visible_after_recovery() {
    use crate::identity::account::Account;
    use crate::node::conversation::account_conversation_id;
    use std::io::Cursor;
    let dir = tempfile::tempdir().unwrap();
    let me = DeviceIdentity::generate();
    let peer = DeviceIdentity::generate();
    let peer_account = Account::generate();
    let roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(
        &roster,
        &peer,
        &peer_account,
        "peer",
        47474,
        &me.public().user_id(),
    );
    let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
    let (channel_tx, _channel_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let source = Node::open(
        me,
        roster,
        dm_tx,
        channel_tx,
        file_tx,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "password",
    )
    .unwrap();
    let contact = peer_account.account_id();
    let conversation = account_conversation_id(&source.account_id(), &contact);
    let manifest = crate::file::FileManifest {
        name: "file.txt".into(),
        size: 4,
        mime: "text/plain".into(),
        checksum: crate::file::file_checksum(b"file"),
        file_key: [7; 32],
        file_conv: ConversationId::new([8; 32]),
        chunk_count: 1,
    };
    let event = crate::eventlog::event::Event::new(
        &peer,
        conversation,
        1,
        vec![],
        1,
        100,
        crate::eventlog::event::EventKind::FileManifest,
        b"sealed file card".to_vec(),
    );
    source.log.lock().unwrap().append(event.clone()).unwrap();
    source
        .received_files
        .lock()
        .unwrap()
        .record(
            conversation,
            peer.public().user_id(),
            100,
            &manifest.encode(),
            event.id,
        )
        .unwrap();
    let mut snapshot = Vec::new();
    source.write_recovery_snapshot(&mut snapshot, 101).unwrap();
    let key = crate::recovery::key::RecoveryKey::generate();
    let mut archive = Vec::new();
    source
        .write_encrypted_recovery_archive(&mut archive, &key, 101)
        .unwrap();
    let preview =
        Node::inspect_encrypted_recovery_archive(Cursor::new(archive), &key, dir.path()).unwrap();
    assert_eq!(preview.messages, 0);
    assert_eq!(preview.file_cards, 1);
    assert_eq!(preview.media_files, 0);
    assert_eq!(preview.missing_media, 1);
    let restored = Node::install_recovery_snapshot(
        Cursor::new(snapshot),
        &dir.path().join("restored"),
        "new password",
    )
    .unwrap();
    assert!(restored.known_account_ids().contains(&contact));
    assert_eq!(restored.history(conversation, 10).len(), 1);
    let mut second_snapshot = Vec::new();
    restored
        .write_recovery_snapshot(&mut second_snapshot, 102)
        .unwrap();
    let twice_restored = Node::install_recovery_snapshot(
        Cursor::new(second_snapshot),
        &dir.path().join("twice-restored"),
        "third password",
    )
    .unwrap();
    assert!(twice_restored.known_account_ids().contains(&contact));
}

#[test]
fn encrypted_recovery_requires_the_matching_key_before_installation() {
    use crate::recovery::{BackupError, archive::ArchiveError, key::RecoveryKey};
    use std::io::Cursor;
    let dir = tempfile::tempdir().unwrap();
    let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
    let (channel_tx, _channel_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let source = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm_tx,
        channel_tx,
        file_tx,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "old password",
    )
    .unwrap();
    let key = RecoveryKey::generate();
    let mut encrypted = Vec::new();
    source
        .write_encrypted_recovery_archive(&mut encrypted, &key, 1234)
        .unwrap();
    let wrong = RecoveryKey::generate();
    let destination = dir.path().join("restored");
    assert!(matches!(
        Node::install_encrypted_recovery_archive(
            Cursor::new(&encrypted),
            &wrong,
            dir.path(),
            &destination,
            "new password"
        ),
        Err(BackupError::Archive(ArchiveError::InvalidArchive))
    ));
    assert!(!destination.exists());
    let preview =
        Node::inspect_encrypted_recovery_archive(Cursor::new(&encrypted), &key, dir.path())
            .unwrap();
    assert_eq!(preview.account_id, source.account_id());
    assert_eq!(preview.created_at_ms, 1234);
    let (restored, installed_preview) = Node::install_encrypted_recovery_archive(
        Cursor::new(&encrypted),
        &key,
        dir.path(),
        &destination,
        "new password",
    )
    .unwrap();
    assert_eq!(restored.account_id(), source.account_id());
    assert_eq!(installed_preview.account_id, preview.account_id);
    assert_ne!(
        restored.identity.public().user_id(),
        source.identity.public().user_id()
    );
}

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

    assert!(
        !bob_node.verified_dm_pull_peer(&alice_uid),
        "a direct incoming DM must not require a verified pull contact"
    );

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
async fn reconnecting_dm_recipient_pulls_from_sender_without_sender_route_or_relay() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let alice_uid = alice.public().user_id();
    let bob_uid = bob.public().user_id();
    let alice_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let alice_port = alice_listener.local_addr().unwrap().port();
    let bob_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let dead = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let bob_old_port = dead.local_addr().unwrap().port();
    drop(dead);
    let alice_roster = seed_roster(&bob, "Bob", bob_old_port, &alice_uid);
    let bob_roster = seed_roster(&alice, "Alice", alice_port, &bob_uid);
    let dir = tempfile::tempdir().unwrap();
    let open = |identity, roster, name: &str| {
        let (dm_tx, dm_rx) = mpsc::unbounded_channel();
        let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
        let (file_tx, _file_rx) = mpsc::unbounded_channel();
        let node = Node::open(
            identity,
            roster,
            dm_tx,
            ch_tx,
            file_tx,
            &dir.path().join(format!("{name}.log")),
            &dir.path().join(format!("{name}-sent.log")),
            "pw",
        )
        .unwrap();
        (node, dm_rx)
    };
    let (alice_node, _alice_rx) = open(alice, alice_roster, "alice");
    let (bob_node, mut bob_rx) = open(bob, bob_roster, "bob");
    alice_node
        .send_dm(&bob_uid, b"offline direct pull")
        .await
        .unwrap();
    assert!(bob_rx.try_recv().is_err());
    let bob_task = tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));
    assert!(
        tokio::time::timeout(Duration::from_secs(1), alice_listener.accept())
            .await
            .is_err(),
        "unverified discovery must not initiate a TCP pull from Alice"
    );
    assert!(
        bob_rx.try_recv().is_err(),
        "discovery alone must not authorize recipient-initiated DM pulls"
    );
    bob_node.replace_verified_dm_pull_peers(vec![alice_uid.clone()]);
    let alice_task = tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));
    let received = tokio::time::timeout(Duration::from_secs(5), bob_rx.recv())
        .await
        .expect("Bob pulls from Alice after reconnecting without a relay")
        .expect("DM stream open");
    assert_eq!(received.from, alice_uid);
    assert_eq!(received.text, b"offline direct pull");
    alice_task.abort();
    bob_task.abort();
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

    bob_node.drain_from_post_office().await;
    assert!(
        bob_rx.try_recv().is_err(),
        "discovery alone must not authorize a relay DM pull"
    );
    bob_node.replace_verified_dm_pull_peers(vec![alice_uid.clone()]);

    // Bob drains from the PO (retry to absorb the relay's async ingest).
    let received = tokio::time::timeout(Duration::from_secs(5), async {
        let mut received = None;
        for _ in 0..25 {
            bob_node.drain_from_post_office().await;
            if let Ok(dm) = tokio::time::timeout(Duration::from_millis(200), bob_rx.recv()).await {
                received = dm;
                break;
            }
        }
        received
    })
    .await
    .expect("post-office drain must finish within the receive budget")
    .expect("Bob received the held DM from the post office");
    assert_eq!(received.from, alice_uid);
    assert_eq!(received.from_name, "Alice");
    assert_eq!(received.text, b"held-hello");

    let conv = dm_conversation_id(&alice_node.identity.public(), &bob_node.identity.public());
    let mailbox = crate::postoffice::opaque::RelayMailbox::new(
        &alice_node.identity,
        &bob_node.identity.public(),
        &po_seed.public(),
    )
    .unwrap();
    let stored = po_store.lock().unwrap().held_events(&mailbox.id());
    assert!(!stored.is_empty());
    assert!(po_store.lock().unwrap().held_ids(&conv).is_empty());
    for outer in &stored {
        assert_eq!(outer.wall_clock, 0);
        assert_eq!(outer.kind, EventKind::Message);
        assert!(outer.parents.is_empty());
        assert_ne!(
            outer.author.ed25519_pub(),
            &alice_node.identity.public().ed25519_pub
        );
        assert!(
            !outer
                .ciphertext
                .windows(b"held-hello".len())
                .any(|w| w == b"held-hello")
        );
    }

    let group = alice_node
        .create_channel("relay-secret-room", vec![bob_node.identity.public()])
        .await
        .unwrap();
    assert!(po_store.lock().unwrap().held_ids(&group).is_empty());
    let group_wrappers = po_store.lock().unwrap().held_events(&mailbox.id());
    assert!(group_wrappers.len() > stored.len());
    assert!(group_wrappers.iter().all(|outer| {
        !outer
            .ciphertext
            .windows(b"relay-secret-room".len())
            .any(|window| window == b"relay-secret-room")
    }));
    alice_node
        .send_channel_message(group, b"relay custody")
        .await
        .unwrap();
    let id = alice_node
        .channel_history(group, 10)
        .into_iter()
        .find(|row| row.text == b"relay custody")
        .unwrap()
        .id;
    assert_eq!(alice_node.channel_custody_count(group, id), Some(0));
    assert_eq!(
        alice_node.channel_post_office_custody_count(group, id),
        Some(1)
    );
    bob_node.drain_from_post_office().await;
    assert!(
        bob_node
            .list_channels()
            .iter()
            .any(|summary| summary.id == group && summary.name == "relay-secret-room")
    );

    // Turning relay use off keeps later messages local while Bob remains offline.
    let held_before = po_store.lock().unwrap().held_ids(&mailbox.id());
    alice_node.set_post_office_enabled(false);
    alice_node.send_dm(&bob_uid, b"direct-only").await.unwrap();
    assert_eq!(
        po_store.lock().unwrap().held_ids(&mailbox.id()),
        held_before
    );
}

#[tokio::test]
async fn channel_custody_inherited_holder_requires_signed_account_authority() {
    use crate::{
        identity::account::Account,
        node::channel_account_authority::{AccountAuthorityControl, AccountGrant},
    };

    let dir = tempfile::tempdir().unwrap();
    let owner = personal_message_node(dir.path(), DeviceIdentity::generate(), Account::generate());
    owner.set_post_office_enabled(false);
    let bob = DeviceIdentity::generate();
    let bob_sibling = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unavailable = listener.local_addr().unwrap();
    drop(listener);
    let bob_proof = Announce::new_with_account(&bob, &bob_account, "Bob", unavailable.port());
    for proof in [
        bob_proof.clone(),
        Announce::new_with_account(
            &bob_sibling,
            &bob_account,
            "Bob sibling",
            unavailable.port(),
        ),
    ] {
        owner
            .roster
            .lock()
            .unwrap()
            .update(&proof, unavailable.ip(), &owner.user_id());
    }
    let channel = owner
        .create_channel("account custody fixture", vec![bob.public()])
        .await
        .unwrap();
    let event = owner
        .enqueue_channel_message_reply_expiring_if(
            channel,
            b"synthetic projected observation",
            None,
            None,
            |commit| commit(),
        )
        .unwrap();
    owner
        .publish_channel_custody(channel, event, &bob_sibling.public(), true, false, false)
        .unwrap();
    assert_eq!(
        owner.channel_custody_count(channel, event),
        Some(0),
        "a certificate cannot enroll a nonmember into a legacy channel"
    );

    let own_proof = owner.own_group_proof();
    let projection = owner.account_group_projection(channel);
    let bindings = vec![own_proof.clone(), bob_proof];
    let grants = bindings
        .iter()
        .enumerate()
        .map(|(index, proof)| {
            let accepted = projection
                .accepted
                .iter()
                .find(|member| member.public == proof.public())
                .unwrap();
            AccountGrant {
                incarnation: [index as u8 + 1; 16],
                pending: false,
                original: proof.clone(),
                membership_event: accepted.membership_event,
                acceptance_event: accepted.acceptance_event,
            }
        })
        .collect();
    let authority = AccountAuthorityControl {
        version: 1,
        previous: None,
        issuer: own_proof,
        owner_account: owner.account.public().ed25519_pub,
        grants,
        revoked: Vec::new(),
        legacy_bindings: bindings,
        account_signature: Vec::new(),
    };
    owner
        .append_event(
            channel,
            EventKind::ChannelAccountAuthority,
            authority.signed_encode(channel, &owner.account),
        )
        .unwrap();
    owner.process_channel(channel);
    {
        let book = owner.channels.lock().unwrap();
        let state = book.state(&channel).unwrap();
        assert_eq!(state.owner_account_id(), Some(owner.account_id().as_str()));
        assert!(!state.members().contains(&bob_sibling.public()));
    }
    assert_eq!(
        owner.channel_custody_count(channel, event),
        Some(1),
        "an accepted account grant admits its certified holder without another device invite"
    );
    owner
        .publish_channel_custody(channel, event, &bob.public(), true, false, false)
        .unwrap();
    assert_eq!(
        owner.channel_custody_count(channel, event),
        Some(1),
        "two certified holders of the same accepted account count once"
    );
    owner
        .remove_channel_account_members_if(channel, &bob_account.account_id(), |commit| commit())
        .await
        .unwrap();
    assert_eq!(
        owner.channel_custody_count(channel, event),
        Some(0),
        "old observations cannot preserve a revoked account's eligibility"
    );
}

#[tokio::test]
async fn channel_custody_post_office_observation_survives_sibling_sync_without_relay() {
    use crate::{identity::account::Account, node::account_devices::AccountDevice};

    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let secret = account.secret_bytes();
    let source = personal_message_node(
        &dir.path().join("source"),
        DeviceIdentity::generate(),
        account,
    );
    let sibling_identity = DeviceIdentity::generate();
    let sibling_secret = sibling_identity.secret_bytes();
    let sibling_dir = dir.path().join("sibling");
    let sibling = personal_message_node(
        &sibling_dir,
        sibling_identity,
        Account::from_secret_bytes(secret),
    );
    for (left, right) in [(&source, &sibling), (&sibling, &source)] {
        left.account_devices
            .lock()
            .unwrap()
            .insert(AccountDevice {
                public: right.identity.public(),
                certificate: left.account.certify(&right.identity.public().ed25519_pub),
            })
            .unwrap();
    }

    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let dead_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = dead_listener.local_addr().unwrap();
    drop(dead_listener);
    source.roster.lock().unwrap().update(
        &Announce::new_with_account(&bob, &bob_account, "offline member", bob_addr.port()),
        bob_addr.ip(),
        &source.user_id(),
    );
    let relay = DeviceIdentity::generate();
    let relay_secret = relay.secret_bytes();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_addr = listener.local_addr().unwrap();
    source.roster.lock().unwrap().update(
        &Announce::new_post_office(&relay, "relay", relay_addr.port()),
        relay_addr.ip(),
        &source.user_id(),
    );
    let office = Arc::new(Mutex::new(
        PostOffice::open(&dir.path().join("office.log"), "pw", relay).unwrap(),
    ));
    let relay_task = tokio::spawn(run_relay_accept_loop(
        DeviceIdentity::from_secret_bytes(relay_secret.0, relay_secret.1),
        listener,
        office,
    ));
    let sibling_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sibling_addr = sibling_listener.local_addr().unwrap();
    source.roster.lock().unwrap().update(
        &Announce::new_with_account(
            &sibling.identity,
            &sibling.account,
            "sibling",
            sibling_addr.port(),
        ),
        sibling_addr.ip(),
        &source.user_id(),
    );
    let sibling_server = tokio::spawn(sibling.clone().run_accept_loop(sibling_listener));
    let channel = source
        .create_channel(
            "relay custody account fixture",
            vec![bob.public(), sibling.identity.public()],
        )
        .await
        .unwrap();
    assert!(
        sibling
            .list_channels()
            .iter()
            .any(|row| row.id == channel && !row.pending_invite),
        "the authorized channel setup must reach the sibling before it goes offline"
    );
    sibling_server.abort();
    let _ = sibling_server.await;
    source.sessions.clear();
    source
        .roster
        .lock()
        .unwrap()
        .forget(&sibling.identity.public());
    source
        .send_channel_message(channel, b"synthetic relay copy observation")
        .await
        .unwrap();
    let event = source.channel_history(channel, 10)[0].id;
    assert_eq!(source.channel_custody_count(channel, event), Some(0));
    assert_eq!(
        source.channel_post_office_custody_count(channel, event),
        Some(1)
    );
    assert!(
        source.channel_custody_observations(channel, event).iter().any(|meta| {
            matches!(&meta.key, super::super::account_documents::AccountDocumentKey::ChannelCustody { holder, .. } if holder.starts_with("relay:")) && !meta.tombstone
        }),
        "actual wrapper custody must publish the private relay observation before sync"
    );

    relay_task.abort();
    let _ = relay_task.await;
    source.sessions.clear();
    assert!(tokio::net::TcpStream::connect(relay_addr).await.is_err());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source_addr = listener.local_addr().unwrap();
    add_account_peer(
        &sibling.roster,
        &source.identity,
        &source.account,
        "source",
        source_addr.port(),
        &sibling.user_id(),
    );
    let server = tokio::spawn(source.clone().run_accept_loop(listener));
    let worker = tokio::spawn(sibling.clone().run_account_history_loop());
    let imported = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let own_message = sibling
                .channel_history(channel, 10)
                .iter()
                .any(|row| row.id == event && row.from_me);
            if own_message
                && sibling.channel_custody_count(channel, event) == Some(0)
                && sibling.channel_post_office_custody_count(channel, event) == Some(1)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    worker.abort();
    let _ = worker.await;
    sibling.sessions.clear();
    server.abort();
    let _ = server.await;
    let members = {
        let book = sibling.channels.lock().unwrap();
        book.state(&channel).map(|state| {
            state
                .members()
                .iter()
                .map(|member| member.user_id())
                .collect::<Vec<_>>()
        })
    };
    assert!(
        imported.is_ok(),
        "sibling relay import: history={:?}, members={:?}, observations={}, copies={:?}/{:?}, sync={:?}",
        sibling
            .channel_history(channel, 10)
            .iter()
            .map(|row| (row.id, row.from_me))
            .collect::<Vec<_>>(),
        members,
        sibling.channel_custody_observations(channel, event).len(),
        sibling.channel_custody_count(channel, event),
        sibling.channel_post_office_custody_count(channel, event),
        sibling.device_sync_status(),
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while Arc::strong_count(&sibling) != 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("retired sibling work must finish before reopening its encrypted stores");
    drop(sibling);
    let reopened = personal_message_node(
        &sibling_dir,
        DeviceIdentity::from_secret_bytes(sibling_secret.0, sibling_secret.1),
        Account::from_secret_bytes(secret),
    );
    assert_eq!(reopened.channel_custody_count(channel, event), Some(0));
    assert_eq!(
        reopened.channel_post_office_custody_count(channel, event),
        Some(1)
    );
}

#[tokio::test]
async fn preferred_post_office_fails_over_and_fallback_mailbox_is_drained_after_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let preferred = DeviceIdentity::generate();
    let fallback = DeviceIdentity::generate();
    let alice_uid = alice.user_id();
    let bob_uid = bob.user_id();

    let offline_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let preferred_addr = offline_listener.local_addr().unwrap();
    drop(offline_listener);
    let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = bob_listener.local_addr().unwrap();
    drop(bob_listener);
    let fallback_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fallback_addr = fallback_listener.local_addr().unwrap();
    let (fallback_ed, fallback_x) = fallback.secret_bytes();
    let fallback_store = Arc::new(Mutex::new(
        PostOffice::open(&dir.path().join("fallback.log"), "pw", fallback).unwrap(),
    ));
    let fallback_task = tokio::spawn(run_relay_accept_loop(
        DeviceIdentity::from_secret_bytes(fallback_ed, fallback_x),
        fallback_listener,
        Arc::clone(&fallback_store),
    ));

    let make_roster = |self_uid: &str, other: &DeviceIdentity, other_addr: SocketAddr| {
        let mut roster = Roster::default();
        for (proof, addr) in [
            (Announce::new(other, "peer", other_addr.port()), other_addr),
            (
                Announce::new_post_office(&preferred, "preferred", preferred_addr.port()),
                preferred_addr,
            ),
            (
                Announce::new_post_office(
                    &DeviceIdentity::from_secret_bytes(fallback_ed, fallback_x),
                    "fallback",
                    fallback_addr.port(),
                ),
                fallback_addr,
            ),
        ] {
            roster.update(&proof, addr.ip(), self_uid);
        }
        Arc::new(Mutex::new(roster))
    };
    let alice_roster = make_roster(&alice_uid, &bob, bob_addr);
    let bob_roster = make_roster(&bob_uid, &alice, bob_addr);
    let open = |name: &str, identity, roster| {
        let (dm, receiver) = mpsc::unbounded_channel();
        let (channels, _) = mpsc::unbounded_channel();
        let (files, _) = mpsc::unbounded_channel();
        let node = Node::open(
            identity,
            roster,
            dm,
            channels,
            files,
            &dir.path().join(format!("{name}.log")),
            &dir.path().join(format!("{name}-sent.log")),
            "pw",
        )
        .unwrap();
        (node, receiver)
    };
    let (alice_node, _) = open("alice", alice, alice_roster);
    let (bob_node, mut bob_rx) = open("bob", bob, bob_roster);
    alice_node
        .set_bridge_hub_id(Some(&preferred.user_id()))
        .unwrap();
    bob_node
        .set_bridge_hub_id(Some(&preferred.user_id()))
        .unwrap();
    alice_node
        .send_dm(&bob_uid, b"fallback-delivery")
        .await
        .unwrap();

    let fallback_mailbox = crate::postoffice::opaque::RelayMailbox::new(
        &alice_node.identity,
        &bob_node.identity.public(),
        &DeviceIdentity::from_secret_bytes(fallback_ed, fallback_x).public(),
    )
    .unwrap();
    assert!(
        !fallback_store
            .lock()
            .unwrap()
            .held_events(&fallback_mailbox.id())
            .is_empty(),
        "the fallback relay must take custody while preferred is offline"
    );

    // Bring the preferred relay back before Bob drains. Reading only the
    // preferred mailbox would strand the message already held by fallback.
    let preferred_listener = TcpListener::bind(preferred_addr).await.unwrap();
    let (preferred_ed, preferred_x) = preferred.secret_bytes();
    let preferred_store = Arc::new(Mutex::new(
        PostOffice::open(&dir.path().join("preferred.log"), "pw", preferred).unwrap(),
    ));
    let preferred_task = tokio::spawn(run_relay_accept_loop(
        DeviceIdentity::from_secret_bytes(preferred_ed, preferred_x),
        preferred_listener,
        preferred_store,
    ));
    bob_node.replace_verified_dm_pull_peers(vec![alice_uid]);
    let received = tokio::time::timeout(Duration::from_secs(5), async {
        let mut received = None;
        for _ in 0..10 {
            bob_node.drain_from_post_office().await;
            if let Ok(dm) = bob_rx.try_recv() {
                received = Some(dm);
                break;
            }
        }
        received
    })
    .await
    .expect("fallback mailbox drain must finish within the receive budget");
    assert_eq!(
        received.expect("fallback mailbox drained").text,
        b"fallback-delivery"
    );
    preferred_task.abort();
    fallback_task.abort();
}

#[tokio::test]
async fn alternate_federated_relay_delivers_when_preferred_is_offline() {
    use crate::postoffice::federation::FederationDirectory;

    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let preferred = DeviceIdentity::generate();
    let fallback = DeviceIdentity::generate();
    let remote = DeviceIdentity::generate();
    let alice_id = alice.user_id();
    let bob_id = bob.user_id();
    let reserved = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let preferred_addr = reserved.local_addr().unwrap();
    drop(reserved);
    let reserved = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = reserved.local_addr().unwrap();
    drop(reserved);
    let fallback_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fallback_addr = fallback_listener.local_addr().unwrap();
    let remote_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = remote_listener.local_addr().unwrap();
    let duplicate = |identity: &DeviceIdentity| {
        let (ed, x) = identity.secret_bytes();
        DeviceIdentity::from_secret_bytes(ed, x)
    };
    let fallback_store = Arc::new(Mutex::new(
        PostOffice::open(&dir.path().join("fallback.log"), "pw", duplicate(&fallback)).unwrap(),
    ));
    let remote_store = Arc::new(Mutex::new(
        PostOffice::open(&dir.path().join("remote.log"), "pw", duplicate(&remote)).unwrap(),
    ));
    let fallback_directory = FederationDirectory::new();
    let fallback_task = tokio::spawn(
        crate::node::postbox::run_federated_relay_accept_loop_with_directory(
            duplicate(&fallback),
            fallback_listener,
            Arc::clone(&fallback_store),
            Arc::new(HashSet::from([remote.user_id()])),
            fallback_directory.clone(),
        ),
    );
    let remote_task = tokio::spawn(
        crate::node::postbox::run_federated_relay_accept_loop_with_directory(
            duplicate(&remote),
            remote_listener,
            Arc::clone(&remote_store),
            Arc::new(HashSet::from([fallback.user_id()])),
            FederationDirectory::new(),
        ),
    );

    let mut alice_roster = Roster::default();
    for (proof, addr) in [
        (Announce::new(&bob, "Bob", bob_addr.port()), bob_addr),
        (
            Announce::new_post_office(&preferred, "preferred", preferred_addr.port()),
            preferred_addr,
        ),
        (
            Announce::new_post_office(&fallback, "fallback", fallback_addr.port()),
            fallback_addr,
        ),
    ] {
        alice_roster.update(&proof, addr.ip(), &alice_id);
    }
    let mut bob_roster = Roster::default();
    bob_roster.update(
        &Announce::new(&alice, "Alice", bob_addr.port()),
        bob_addr.ip(),
        &bob_id,
    );
    bob_roster.update(
        &Announce::new_post_office(&remote, "remote", remote_addr.port()),
        remote_addr.ip(),
        &bob_id,
    );
    let open = |name: &str, identity, roster| {
        let (dm, rx) = mpsc::unbounded_channel();
        let (channels, _) = mpsc::unbounded_channel();
        let (files, _) = mpsc::unbounded_channel();
        let node = Node::open(
            identity,
            Arc::new(Mutex::new(roster)),
            dm,
            channels,
            files,
            &dir.path().join(format!("{name}.log")),
            &dir.path().join(format!("{name}-sent.log")),
            "pw",
        )
        .unwrap();
        (node, rx)
    };
    let (alice_node, _) = open("alice", alice, alice_roster);
    let (bob_node, mut bob_rx) = open("bob", bob, bob_roster);
    alice_node.set_bridge_discovery_enabled(true);
    bob_node.set_bridge_discovery_enabled(true);
    alice_node
        .set_bridge_hub_id(Some(&preferred.user_id()))
        .unwrap();
    bob_node.replace_verified_dm_pull_peers(vec![alice_id]);

    alice_node
        .send_dm(&bob_id, b"remote-failover")
        .await
        .unwrap();
    assert!(
        !fallback_store
            .lock()
            .unwrap()
            .federated_inventory()
            .is_empty()
    );
    assert!(
        remote_store
            .lock()
            .unwrap()
            .federated_inventory()
            .is_empty()
    );
    bob_node.drain_from_post_office().await;
    assert!(
        bob_rx.try_recv().is_err(),
        "no cross-site route has run yet"
    );
    fallback_directory
        .sync_with_relay(remote_addr, &fallback, &remote.public(), &fallback_store)
        .await
        .unwrap();
    assert!(
        !remote_store
            .lock()
            .unwrap()
            .federated_inventory()
            .is_empty()
    );
    bob_node.drain_from_post_office().await;
    assert_eq!(bob_rx.try_recv().unwrap().text, b"remote-failover");

    fallback_task.abort();
    remote_task.abort();
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
            conv,
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

    let request = OutgoingMessageRequest {
        request_id: [91; 32],
        destination: MessageDestination::Channel(channel),
        text: b"bot alert".to_vec(),
        reply_to: None,
        expires_at_ms: None,
    };
    let accepted = alice_node
        .enqueue_message_once_if(&request, |accept| accept())
        .await
        .unwrap();
    alice_node
        .flush_accepted_message(&request.destination, accepted.message_id)
        .await;
    let alert = tokio::time::timeout(std::time::Duration::from_secs(5), bob_ch_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alert.text, b"bot alert");
    assert_eq!(alert.from, alice_node.user_id());
    assert!(
        alice_node
            .enqueue_message_once_if(&request, |accept| accept())
            .await
            .unwrap()
            .duplicate
    );
    assert_eq!(bob_node.channel_history(channel, 10).len(), 2);

    // The membership projection can lag the durable log while two commands
    // interleave. The transfer must not report success if replay sees a newer
    // removal first and rejects the target.
    let owner = alice_node.user_id();
    alice_node
        .append_event(
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "general".into(),
                members: vec![alice_node.identity.public()],
                epoch: 1,
                owner: owner.clone(),
            }
            .encode(),
        )
        .unwrap();
    assert!(
        alice_node
            .transfer_channel_owner(channel, &bob_node.user_id())
            .await
            .is_err(),
        "a rejected transfer must not report success"
    );
    assert_eq!(alice_node.channel_owner(channel), owner);
}

#[tokio::test]
async fn stalled_channel_peer_does_not_delay_healthy_member() {
    let alice = DeviceIdentity::generate();
    let mut slow = DeviceIdentity::generate();
    let mut fast = DeviceIdentity::generate();
    if slow.public().user_id() > fast.public().user_id() {
        std::mem::swap(&mut slow, &mut fast);
    }
    let dir = tempfile::tempdir().unwrap();
    let slow_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let slow_port = slow_listener.local_addr().unwrap().port();
    let fast_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let fast_port = fast_listener.local_addr().unwrap().port();
    let alice_roster = seed_roster(&slow, "Slow", slow_port, &alice.public().user_id());
    add_peer(
        &alice_roster,
        &fast,
        "Fast",
        fast_port,
        &alice.public().user_id(),
    );
    let open = |identity, roster, name: &str| {
        let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
        let (ch_tx, ch_rx) = mpsc::unbounded_channel();
        let (file_tx, _file_rx) = mpsc::unbounded_channel();
        let node = Node::open(
            identity,
            roster,
            dm_tx,
            ch_tx,
            file_tx,
            &dir.path().join(format!("{name}.log")),
            &dir.path().join(format!("{name}-sent.log")),
            "pw",
        )
        .unwrap();
        (node, ch_rx)
    };
    let (alice_node, _alice_rx) = open(alice, alice_roster, "alice");
    let (slow_node, _slow_rx) = open(slow, Arc::new(Mutex::new(Roster::default())), "slow");
    let (fast_node, mut fast_rx) = open(fast, Arc::new(Mutex::new(Roster::default())), "fast");
    let slow_task = tokio::spawn(Arc::clone(&slow_node).run_accept_loop(slow_listener));
    let fast_task = tokio::spawn(Arc::clone(&fast_node).run_accept_loop(fast_listener));
    let channel = alice_node
        .create_channel(
            "stalled-peer",
            vec![slow_node.identity.public(), fast_node.identity.public()],
        )
        .await
        .unwrap();

    slow_task.abort();
    let _ = slow_task.await;
    let stalled_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, slow_port))
        .await
        .unwrap();
    let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
    let stalled_task = tokio::spawn(async move {
        let (socket, _) = stalled_listener.accept().await.unwrap();
        let _ = accepted_tx.send(());
        tokio::time::sleep(Duration::from_secs(20)).await;
        drop(socket);
    });

    let sender = tokio::spawn({
        let node = Arc::clone(&alice_node);
        async move {
            node.send_channel_message(channel, b"healthy peer first")
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), accepted_rx)
        .await
        .expect("sender contacted the stalled peer")
        .expect("stalled listener accepted a connection");
    let received = tokio::time::timeout(Duration::from_secs(5), fast_rx.recv())
        .await
        .expect("healthy peer must receive before the stalled handshake times out")
        .expect("channel stream open");
    assert_eq!(received.text, b"healthy peer first");
    let status_sender = tokio::spawn({
        let node = Arc::clone(&alice_node);
        async move { node.set_channel_replica_enabled(channel, false).await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while fast_node.channel_replica_enabled(channel, &alice_node.user_id()) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("healthy peer must receive a control event despite the stalled peer");
    stalled_task.abort();
    sender.await.unwrap().unwrap();
    status_sender.await.unwrap().unwrap();
    fast_task.abort();
}

#[tokio::test]
async fn stalled_recovery_peer_does_not_delay_healthy_replica() {
    let alice = DeviceIdentity::generate();
    let mut slow = DeviceIdentity::generate();
    let mut fast = DeviceIdentity::generate();
    if slow.public().user_id() > fast.public().user_id() {
        std::mem::swap(&mut slow, &mut fast);
    }
    let dir = tempfile::tempdir().unwrap();
    let slow_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let slow_port = slow_listener.local_addr().unwrap().port();
    let fast_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let fast_port = fast_listener.local_addr().unwrap().port();
    let alice_roster = seed_roster(&slow, "Slow", slow_port, &alice.public().user_id());
    add_peer(
        &alice_roster,
        &fast,
        "Fast",
        fast_port,
        &alice.public().user_id(),
    );
    let open = |identity, roster, name: &str| {
        let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
        let (ch_tx, ch_rx) = mpsc::unbounded_channel();
        let (file_tx, _file_rx) = mpsc::unbounded_channel();
        let node = Node::open(
            identity,
            roster,
            dm_tx,
            ch_tx,
            file_tx,
            &dir.path().join(format!("{name}.log")),
            &dir.path().join(format!("{name}-sent.log")),
            "pw",
        )
        .unwrap();
        (node, ch_rx)
    };
    let (alice_node, mut alice_rx) = open(alice, alice_roster, "alice");
    let (slow_node, _slow_rx) = open(slow, Arc::new(Mutex::new(Roster::default())), "slow");
    let (fast_node, _fast_rx) = open(fast, Arc::new(Mutex::new(Roster::default())), "fast");
    let slow_task = tokio::spawn(Arc::clone(&slow_node).run_accept_loop(slow_listener));
    let fast_task = tokio::spawn(Arc::clone(&fast_node).run_accept_loop(fast_listener));
    let channel = alice_node
        .create_channel(
            "stalled-peer",
            vec![slow_node.identity.public(), fast_node.identity.public()],
        )
        .await
        .unwrap();

    slow_task.abort();
    let _ = slow_task.await;
    let stalled_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, slow_port))
        .await
        .unwrap();
    let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
    let stalled_task = tokio::spawn(async move {
        let (socket, _) = stalled_listener.accept().await.unwrap();
        let _ = accepted_tx.send(());
        tokio::time::sleep(Duration::from_secs(20)).await;
        drop(socket);
    });

    fast_node
        .send_channel_message(channel, b"recover promptly")
        .await
        .unwrap();
    let recovery = tokio::spawn({
        let node = Arc::clone(&alice_node);
        async move { node.pull_one_channel_from_member(0).await }
    });
    tokio::time::timeout(Duration::from_secs(5), accepted_rx)
        .await
        .unwrap()
        .unwrap();
    let received = tokio::time::timeout(Duration::from_secs(5), alice_rx.recv())
        .await
        .expect("healthy recovery must progress before a stalled peer times out")
        .unwrap();
    assert_eq!(received.text, b"recover promptly");
    tokio::time::timeout(Duration::from_secs(1), recovery)
        .await
        .expect("successful recovery must free its group lane before the stalled timeout")
        .unwrap();
    stalled_task.abort();
    fast_task.abort();
}

/// A restarted member recovers from another member when the author and post office
/// are unavailable. Discovery of the serving member wakes recovery immediately.
#[tokio::test]
async fn channel_member_recovers_missed_message_from_peer() {
    let alice = DeviceIdentity::generate();
    let alice_secrets = alice.secret_bytes();
    let bob = DeviceIdentity::generate();
    let carol = DeviceIdentity::generate();
    let carol_secrets = carol.secret_bytes();
    let alice_id = alice.public().user_id();
    let carol_id = carol.public().user_id();
    let dir = tempfile::tempdir().unwrap();
    let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_port = bob_listener.local_addr().unwrap().port();
    let carol_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let carol_port = carol_listener.local_addr().unwrap().port();

    let alice_roster = seed_roster(&bob, "Bob", bob_port, &alice_id);
    add_peer(&alice_roster, &carol, "Carol", carol_port, &alice_id);
    let bob_roster = Arc::new(Mutex::new(Roster::default()));
    let carol_roster = Arc::new(Mutex::new(Roster::default()));
    let carol_roster_for_discovery = Arc::clone(&carol_roster);
    let bob_pub = bob.public();
    let carol_pub = carol.public();
    let open = |identity, roster, name: &str| {
        let node_dir = dir.path().join(name);
        std::fs::create_dir_all(&node_dir).unwrap();
        let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
        let (ch_tx, ch_rx) = mpsc::unbounded_channel();
        let (file_tx, _file_rx) = mpsc::unbounded_channel();
        let node = Node::open(
            identity,
            roster,
            dm_tx,
            ch_tx,
            file_tx,
            &node_dir.join("messages.log"),
            &node_dir.join("sent.log"),
            "pw",
        )
        .unwrap();
        (node, ch_rx)
    };
    let (alice_node, _alice_rx) = open(alice, alice_roster, "alice");
    let (bob_node, mut bob_rx) = open(bob, bob_roster, "bob");
    let (carol_node, carol_rx) = open(carol, carol_roster, "carol");
    let bob_task = tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));
    let carol_task = tokio::spawn(Arc::clone(&carol_node).run_accept_loop(carol_listener));

    let channel = alice_node
        .create_channel("general", vec![bob_pub, carol_pub.clone()])
        .await
        .unwrap();
    assert!(
        carol_node
            .list_channels()
            .iter()
            .any(|item| item.id == channel)
    );
    carol_task.abort();
    carol_task.await.unwrap_err();
    drop(carol_node);
    drop(carol_rx);
    alice_node.roster.lock().unwrap().forget(&carol_pub);
    assert!(alice_node.routing_peer(&carol_id).is_none());

    alice_node
        .send_channel_message(channel, b"recover from Bob")
        .await
        .unwrap();
    let sent_id = alice_node.channel_history(channel, 10)[0].id;
    assert_eq!(alice_node.channel_custody_count(channel, sent_id), Some(1));
    assert_eq!(
        alice_node.channel_custody_count(channel, EventId::new([0; 32])),
        None
    );
    drop(alice_node);
    let (reopened_alice, _alice_rx) = open(
        DeviceIdentity::from_secret_bytes(alice_secrets.0, alice_secrets.1),
        Arc::new(Mutex::new(Roster::default())),
        "alice",
    );
    assert_eq!(
        reopened_alice.channel_custody_count(channel, sent_id),
        Some(1)
    );
    drop(reopened_alice);
    tokio::time::timeout(Duration::from_secs(5), bob_rx.recv())
        .await
        .expect("Bob received the original push");
    let (carol_node, mut carol_rx) = open(
        DeviceIdentity::from_secret_bytes(carol_secrets.0, carol_secrets.1),
        Arc::clone(&carol_roster_for_discovery),
        "carol",
    );
    let before_recovery = carol_node.channel_history(channel, 10);
    assert!(before_recovery.is_empty(), "{before_recovery:?}");
    assert!(
        !carol_node.verified_dm_pull_peer(&bob_node.user_id()),
        "channel recovery must not depend on DM safety-number verification"
    );

    let changed = carol_roster_for_discovery.lock().unwrap().changes();
    let recovery_task = tokio::spawn(Arc::clone(&carol_node).run_channel_recovery_loop(changed));
    add_peer(
        &carol_roster_for_discovery,
        &bob_node.identity,
        "Bob",
        bob_port,
        &carol_id,
    );
    let recovered = tokio::time::timeout(Duration::from_secs(5), carol_rx.recv())
        .await
        .expect("Carol recovered from Bob")
        .expect("channel stream open");
    assert_eq!(recovered.text, b"recover from Bob");
    assert_eq!(recovered.arrival_kind, "catch_up");
    assert!(recovered.received_at > 0);
    assert_eq!(recovered.event_id, sent_id);
    assert!(!recovered.batch_complete);
    assert!(!recovered.reception_only);
    let completed = carol_rx.try_recv().unwrap();
    assert!(completed.reception_only && completed.batch_complete);
    assert!(completed.text.is_empty() && completed.wrapped.is_empty());
    assert_eq!(completed.batch_id, recovered.batch_id);
    assert!(recovered.batch_id.is_some());
    let recovery = carol_node.channel_recovery(channel).unwrap();
    assert!(matches!(recovery.phase, "peer_checked" | "catching_up"));
    assert!(recovery.checked_at.is_some());
    assert_eq!(carol_node.channel_history(channel, 10).len(), 1);
    let resumed_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, carol_port))
        .await
        .unwrap();
    let resumed_task = tokio::spawn(Arc::clone(&carol_node).run_accept_loop(resumed_listener));
    let (reopened_alice, _alice_rx) = open(
        DeviceIdentity::from_secret_bytes(alice_secrets.0, alice_secrets.1),
        Arc::new(Mutex::new(Roster::default())),
        "alice",
    );
    let alice_changes = reopened_alice.roster.lock().unwrap().changes();
    let alice_recovery =
        tokio::spawn(Arc::clone(&reopened_alice).run_channel_recovery_loop(alice_changes));
    add_peer(
        &reopened_alice.roster,
        &carol_node.identity,
        "Carol",
        carol_port,
        &alice_id,
    );
    tokio::time::timeout(Duration::from_secs(8), async {
        while reopened_alice.channel_custody_count(channel, sent_id) != Some(2) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("restart retries the missing second member copy");
    alice_recovery.abort();
    resumed_task.abort();
    reopened_alice
        .roster
        .lock()
        .unwrap()
        .forget(&carol_node.identity.public());
    reopened_alice
        .send_channel_message(channel, b"local only")
        .await
        .unwrap();
    let local_id = reopened_alice
        .channel_history(channel, 10)
        .last()
        .unwrap()
        .id;
    assert_eq!(
        reopened_alice.channel_custody_count(channel, local_id),
        Some(0)
    );
    recovery_task.abort();
    bob_task.abort();
}

/// Real Noise/TCP sockets and complete Nodes, with deterministic signed routes.
/// Run each size with MESH_TALK_CHANNEL_MEMBERS=5, 20, or 50. Multicast and
/// separate physical hosts still require a device trial.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real TCP scale rehearsal; run with MESH_TALK_CHANNEL_MEMBERS=5/20/50"]
async fn channel_real_tcp_scale_rehearsal() {
    let count: usize = std::env::var("MESH_TALK_CHANNEL_MEMBERS")
        .unwrap_or_else(|_| "5".into())
        .parse()
        .unwrap();
    assert!([5, 20, 50].contains(&count));
    let started = std::time::Instant::now();
    let dir = tempfile::tempdir().unwrap();
    let identities: Vec<_> = (0..count).map(|_| DeviceIdentity::generate()).collect();
    let public: Vec<_> = identities.iter().map(DeviceIdentity::public).collect();
    let mut listeners = Vec::new();
    let mut ports = Vec::new();
    for _ in 0..count {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        ports.push(listener.local_addr().unwrap().port());
        listeners.push(listener);
    }
    let rosters: Vec<_> = (0..count)
        .map(|_| Arc::new(Mutex::new(Roster::default())))
        .collect();
    for i in 0..count {
        for j in 0..count {
            if i != j {
                add_peer(
                    &rosters[i],
                    &identities[j],
                    &format!("member-{j}"),
                    ports[j],
                    &public[i].user_id(),
                );
            }
        }
    }
    let mut receivers = Vec::new();
    let mut nodes = Vec::new();
    for (i, identity) in identities.into_iter().enumerate() {
        let path = dir.path().join(format!("member-{i}"));
        std::fs::create_dir_all(&path).unwrap();
        let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
        let (ch_tx, ch_rx) = mpsc::unbounded_channel();
        let (file_tx, _file_rx) = mpsc::unbounded_channel();
        receivers.push(ch_rx);
        nodes.push(
            Node::open(
                identity,
                Arc::clone(&rosters[i]),
                dm_tx,
                ch_tx,
                file_tx,
                &path.join("messages.log"),
                &path.join("sent.log"),
                "pw",
            )
            .unwrap(),
        );
    }
    let mut tasks: Vec<_> = listeners
        .into_iter()
        .enumerate()
        .map(|(i, listener)| tokio::spawn(Arc::clone(&nodes[i]).run_accept_loop(listener)))
        .collect();
    let channel = tokio::time::timeout(
        Duration::from_secs(90),
        nodes[0].create_channel("scale", public[1..].to_vec()),
    )
    .await
    .expect("channel creation deadline")
    .unwrap();
    assert!(
        nodes
            .iter()
            .all(|node| node.list_channels().iter().any(|item| item.id == channel))
    );
    // In a five-member channel everyone is a direct target, so take one
    // listener down before the send to exercise actual missed-message recovery.
    if count == 5 {
        tasks[4].abort();
        let _ = (&mut tasks[4]).await;
    }
    let send_started = std::time::Instant::now();
    nodes[0]
        .send_channel_message(channel, b"scale-check")
        .await
        .unwrap();
    let sender_ms = send_started.elapsed().as_millis();
    let event_id = nodes[0].channel_history(channel, 10)[0].id;
    let (contacts, sender_frame_bytes) = nodes[0].channel_last_send.lock().unwrap().unwrap();
    assert_eq!(contacts, if count <= 5 { count - 1 } else { 2 });
    assert!(sender_frame_bytes > 0);
    let initially_holding: Vec<_> = (1..count)
        .filter(|&i| nodes[i].log.lock().unwrap().has(&event_id))
        .collect();
    assert!(!initially_holding.is_empty());
    let mut offline: Vec<_> = if count == 5 {
        vec![4]
    } else {
        (1..count)
            .filter(|i| i % 5 == 4 && !initially_holding.contains(i))
            .collect()
    };
    if initially_holding.iter().all(|i| offline.contains(i)) {
        offline.retain(|i| *i != initially_holding[0]);
    }
    for &i in &offline {
        if count != 5 {
            tasks[i].abort();
        }
    }
    tasks[0].abort();
    for &i in offline.iter().chain(std::iter::once(&0)) {
        if i != 4 || count != 5 {
            let _ = (&mut tasks[i]).await;
        }
    }
    let converge_started = std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            if (1..count).filter(|i| !offline.contains(i)).all(|i| {
                nodes[i]
                    .channel_history(channel, 10)
                    .iter()
                    .any(|entry| entry.text == b"scale-check")
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("online members converged without the sender");
    let online_ms = converge_started.elapsed().as_millis();
    let offline_started = std::time::Instant::now();
    for &i in &offline {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, ports[i]))
            .await
            .unwrap();
        tasks[i] = tokio::spawn(Arc::clone(&nodes[i]).run_accept_loop(listener));
    }
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            if offline.iter().all(|&i| {
                nodes[i]
                    .channel_history(channel, 10)
                    .iter()
                    .any(|entry| entry.text == b"scale-check")
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("offline members recovered from replicas");
    eprintln!(
        "channel-tcp members={count} contacts={contacts} initial_holders={} offline={} sender_frame_bytes={sender_frame_bytes} send_ms={sender_ms} online_converge_ms={online_ms} offline_recovery_ms={} total_ms={}",
        initially_holding.len(),
        offline.len(),
        offline_started.elapsed().as_millis(),
        started.elapsed().as_millis(),
    );
    for task in tasks {
        task.abort();
    }
    drop(receivers);
}

#[tokio::test]
async fn signed_channel_replica_preference_survives_restart_and_ignores_outsiders() {
    let identity = DeviceIdentity::generate();
    let secrets = identity.secret_bytes();
    let dir = tempfile::tempdir().unwrap();
    let open = |identity| {
        let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
        let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
        let (file_tx, _file_rx) = mpsc::unbounded_channel();
        Node::open(
            identity,
            Arc::new(Mutex::new(Roster::default())),
            dm_tx,
            ch_tx,
            file_tx,
            &dir.path().join("messages.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap()
    };
    let node = open(identity);
    let channel = node.create_channel("replicas", vec![]).await.unwrap();
    assert!(node.channel_replica_enabled(channel, &node.user_id()));
    node.set_channel_replica_enabled(channel, false)
        .await
        .unwrap();
    let outsider = DeviceIdentity::generate();
    let (parents, lamport) = node.log.lock().unwrap().prepare(&channel).unwrap();
    let forged = Event::new(
        &outsider,
        channel,
        1,
        parents,
        lamport,
        1,
        EventKind::ChannelReplica,
        vec![0],
    );
    node.log.lock().unwrap().append(forged).unwrap();
    node.process_channel(channel);
    assert!(node.channel_replica_enabled(channel, &outsider.public().user_id()));
    drop(node);
    let reopened = open(DeviceIdentity::from_secret_bytes(secrets.0, secrets.1));
    assert!(!reopened.channel_replica_enabled(channel, &reopened.user_id()));
    assert!(reopened.channel_replica_enabled(channel, &outsider.public().user_id()));
}

/// In a large group Alice knows only Bob's address, while Dana knows no route
/// at all. Bob must forward the channel and later message after receiving them.
#[tokio::test]
async fn large_channel_member_forwards_to_peer_without_author_route() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let dana = DeviceIdentity::generate();
    let extra: Vec<_> = (0..3)
        .map(|_| DeviceIdentity::generate().public())
        .collect();
    let alice_id = alice.public().user_id();
    let bob_id = bob.public().user_id();
    let dir = tempfile::tempdir().unwrap();
    let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_port = bob_listener.local_addr().unwrap().port();
    let dana_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dana_port = dana_listener.local_addr().unwrap().port();
    let alice_roster = seed_roster(&bob, "Bob", bob_port, &alice_id);
    let bob_roster = seed_roster(&dana, "Dana", dana_port, &bob_id);
    let dana_roster = Arc::new(Mutex::new(Roster::default()));
    let bob_pub = bob.public();
    let dana_pub = dana.public();
    let open = |identity, roster, name: &str| {
        let path = dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
        let (ch_tx, ch_rx) = mpsc::unbounded_channel();
        let (file_tx, _file_rx) = mpsc::unbounded_channel();
        let node = Node::open(
            identity,
            roster,
            dm_tx,
            ch_tx,
            file_tx,
            &path.join("messages.log"),
            &path.join("sent.log"),
            "pw",
        )
        .unwrap();
        (node, ch_rx)
    };
    let (alice_node, _alice_rx) = open(alice, alice_roster, "alice");
    let (bob_node, _bob_rx) = open(bob, bob_roster, "bob");
    let (dana_node, mut dana_rx) = open(dana, dana_roster, "dana");
    let bob_task = tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));
    let dana_task = tokio::spawn(Arc::clone(&dana_node).run_accept_loop(dana_listener));

    let mut members = vec![bob_pub, dana_pub];
    members.extend(extra);
    let channel = alice_node.create_channel("relay", members).await.unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        while !dana_node
            .list_channels()
            .iter()
            .any(|item| item.id == channel)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Bob forwarded the channel membership to Dana");

    alice_node
        .send_channel_message(channel, b"via Bob")
        .await
        .unwrap();
    let forwarded = tokio::time::timeout(Duration::from_secs(8), dana_rx.recv())
        .await
        .expect("Bob forwarded the message to Dana")
        .expect("Dana's channel stream open");
    assert_eq!(forwarded.text, b"via Bob");
    add_peer(
        &dana_node.roster,
        &bob_node.identity,
        "Bob",
        bob_port,
        &dana_node.user_id(),
    );
    bob_node
        .roster
        .lock()
        .unwrap()
        .forget(&dana_node.identity.public());

    bob_node
        .set_channel_replica_enabled(channel, false)
        .await
        .unwrap();
    assert!(!bob_node.channel_replica_enabled(channel, &bob_node.user_id()));
    assert!(dana_node.channel_replica_enabled(channel, &bob_node.user_id()));
    add_peer(
        &bob_node.roster,
        &dana_node.identity,
        "Dana",
        dana_port,
        &bob_id,
    );
    bob_node.retry_channel_controls().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while dana_node.channel_replica_enabled(channel, &bob_node.user_id()) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Dana sees Bob's signed replica preference");
    bob_node
        .roster
        .lock()
        .unwrap()
        .forget(&dana_node.identity.public());
    alice_node
        .send_channel_message(channel, b"Bob does not relay")
        .await
        .unwrap();
    dana_node.pull_one_channel_from_member(0).await;
    assert_eq!(dana_node.channel_history(channel, 10).len(), 1);

    // Bob resumes sharing, but has no direct address for Dana: only Dana's pull
    // from Bob can deliver the missed second message.
    bob_node
        .set_channel_replica_enabled(channel, true)
        .await
        .unwrap();
    dana_node.pull_one_channel_from_member(0).await;
    assert_eq!(dana_node.channel_history(channel, 10).len(), 2);
    bob_task.abort();
    dana_task.abort();
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
    assert!(
        alice_hist
            .iter()
            .any(|h| !h.from_me && h.text == b"hi alice" && h.who == bob_node.user_id())
    );
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
    async fn next_message(
        receiver: &mut mpsc::UnboundedReceiver<crate::node::channel::ReceivedChannelMessage>,
    ) -> Option<crate::node::channel::ReceivedChannelMessage> {
        loop {
            let event = receiver.recv().await?;
            if !event.reception_only {
                return Some(event);
            }
        }
    }
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

    let alice_task = tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));
    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));
    tokio::spawn(Arc::clone(&carol_node).run_accept_loop(carol_listener));

    // Epoch 0: {Alice, Bob}. Alice sends; Bob receives.
    let channel = alice_node
        .create_channel_invited("general", vec![bob_pub])
        .await
        .unwrap();
    assert!(
        alice_node
            .transfer_channel_owner(channel, &bob_uid)
            .await
            .is_err(),
        "an invitee cannot own the channel before accepting"
    );
    assert_eq!(alice_node.channel_owner(channel), alice_uid);
    bob_node.accept_channel_invite(channel).await.unwrap();
    alice_node
        .send_channel_message(channel, b"epoch0 msg")
        .await
        .unwrap();
    let got0 = tokio::time::timeout(Duration::from_secs(5), next_message(&mut b_ch_r))
        .await
        .expect("bob received the epoch-0 message within 5s")
        .expect("channel stream open");
    assert_eq!(got0.text, b"epoch0 msg");

    // Add Carol (epoch 1). She gets the MembershipChange via Alice's distribution.
    alice_node
        .add_channel_member(channel, carol_pub)
        .await
        .unwrap();
    assert!(
        carol_node
            .list_channels()
            .iter()
            .any(|summary| summary.id == channel && summary.pending_invite)
    );
    carol_node.accept_channel_invite(channel).await.unwrap();

    // Epoch 1: Alice sends. Her lazy SKD for epoch 1 is distributed before the seal,
    // so both Bob (still a member) and Carol (newly joined) can read it.
    alice_node
        .send_channel_message(channel, b"epoch1 msg")
        .await
        .unwrap();

    let got_carol = tokio::time::timeout(Duration::from_secs(5), next_message(&mut c_ch_r))
        .await
        .expect("carol received the epoch-1 message within 5s")
        .expect("channel stream open");
    assert_eq!(got_carol.text, b"epoch1 msg");
    assert_eq!(got_carol.from, alice_node.user_id());
    add_peer(
        &bob_node.roster,
        &carol_node.identity,
        "Carol",
        carol_addr.port(),
        &bob_uid,
    );
    bob_node
        .send_channel_message(channel, b"bob after invite")
        .await
        .unwrap();
    let from_existing_member =
        tokio::time::timeout(Duration::from_secs(5), next_message(&mut c_ch_r))
            .await
            .expect("new member receives an established member after accepting")
            .expect("channel stream open");
    assert_eq!(from_existing_member.text, b"bob after invite");

    // Late-join isolation: Carol never receives the epoch-0 message (she joined at
    // epoch 1). Her history holds only the epoch-1 message.
    let carol_hist = carol_node.channel_history(channel, 10);
    assert!(
        carol_hist.iter().all(|h| h.text != b"epoch0 msg"),
        "late joiner must not read pre-join messages"
    );
    assert!(carol_hist.iter().any(|h| h.text == b"epoch1 msg"));
    assert!(
        std::iter::from_fn(|| c_ch_r.try_recv().ok()).all(|event| event.reception_only),
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

    // Removing the immutable owner would strand the remaining members without
    // anyone authorized to manage the channel.
    let owner_self_remove = alice_node.remove_channel_member(channel, &alice_uid).await;
    assert!(matches!(owner_self_remove, Err(NodeError::Channel(_))));
    assert!(
        alice_node
            .channel_members(channel)
            .iter()
            .any(|member| member.user_id() == alice_uid)
    );

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
    alice_node
        .transfer_channel_owner(channel, &bob_uid)
        .await
        .unwrap();
    assert_eq!(bob_node.channel_owner(channel), bob_uid);
    bob_node
        .add_channel_member(channel, carol_node.identity.public())
        .await
        .unwrap();
    assert!(
        alice_node
            .channel_members(channel)
            .iter()
            .any(|member| member.user_id() == carol_uid)
    );
    bob_node
        .transfer_channel_owner(channel, &alice_uid)
        .await
        .unwrap();
    // Stop Alice's listener AND recovery loops. Replacing only Bob's dial route
    // leaves Alice online: she can still pull Bob's leave from Bob or Carol.
    alice_task.abort();
    assert!(alice_task.await.unwrap_err().is_cancelled());
    alice_node.sessions.clear();
    tokio::time::timeout(Duration::from_secs(5), alice_node.runtime_work.drain())
        .await
        .expect("offline owner's accepted work must stop");
    // The signed leave remains durable while its owner is actually offline.
    bob_node.leave_channel(channel).await.unwrap();
    assert!(
        bob_node
            .list_channels()
            .iter()
            .any(|summary| summary.id == channel && summary.left)
    );
    assert!(
        alice_node
            .channel_members(channel)
            .iter()
            .any(|member| member.user_id() == bob_uid)
    );
    let resumed_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let resumed_addr = resumed_listener.local_addr().unwrap();
    let resumed_task = tokio::spawn(Arc::clone(&alice_node).run_accept_loop(resumed_listener));
    add_peer(
        &bob_node.roster,
        &alice_node.identity,
        "Alice",
        resumed_addr.port(),
        &bob_uid,
    );
    bob_node.retry_channel_departures().await;
    assert!(
        alice_node
            .channel_members(channel)
            .iter()
            .all(|member| member.user_id() != bob_uid)
    );
    resumed_task.abort();
    assert!(resumed_task.await.unwrap_err().is_cancelled());
    alice_node.sessions.clear();
    alice_node.runtime_work.drain().await;
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
        assert!(got.received_at > 0);
        assert_eq!(got.arrival_kind, "live");
        assert!(!got.own_device);
        assert_eq!(got.batch_size, 1);
        assert!(got.batch_complete);
        assert!(
            bob_node
                .channel_history(channel, 10)
                .iter()
                .any(|entry| entry.id == got.event_id && entry.wall_clock == got.wall_clock)
        );
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
    // Rebuilding the receiver projection (as at restart) must not replay alerts
    // for plaintext already saved locally.
    *bob_node.channels.lock().unwrap() = crate::node::channel::ChannelBook::default();
    bob_node.process_channel(channel);
    assert!(
        b_ch_r.try_recv().is_err(),
        "saved history generated a second reception callback"
    );

    // Losing the projection write must retain the exact decrypted plaintext;
    // recovering the disk is enough, with no new sender event or reachable peer.
    bob_node
        .received
        .lock()
        .unwrap()
        .fail_appends_for_test(true)
        .unwrap();
    alice_node
        .send_channel_message(channel, b"retry after disk recovery")
        .await
        .unwrap();
    assert!(b_ch_r.try_recv().is_err());
    assert_eq!(bob_node.channel_history(channel, 10).len(), 2);
    bob_node
        .received
        .lock()
        .unwrap()
        .fail_appends_for_test(false)
        .unwrap();
    let changes = bob_node.roster.lock().unwrap().changes();
    let recovery_task = tokio::spawn(Arc::clone(&bob_node).run_channel_recovery_loop(changes));
    let retried = tokio::time::timeout(Duration::from_secs(5), b_ch_r.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retried.text, b"retry after disk recovery");
    assert_eq!(retried.arrival_kind, "catch_up");
    let completed = b_ch_r.try_recv().unwrap();
    assert!(completed.reception_only && completed.batch_complete);
    assert!(completed.text.is_empty() && completed.wrapped.is_empty());
    assert_eq!(bob_node.channel_history(channel, 10).len(), 3);
    bob_node.process_channel(channel);
    assert!(b_ch_r.try_recv().is_err(), "retry must surface once");
    bob_node
        .received
        .lock()
        .unwrap()
        .fail_appends_for_test(true)
        .unwrap();
    alice_node
        .send_channel_message(channel, b"erase before retry")
        .await
        .unwrap();
    let erased = alice_node
        .channel_history(channel, 10)
        .into_iter()
        .find(|entry| entry.text == b"erase before retry")
        .unwrap()
        .id;
    bob_node
        .received
        .lock()
        .unwrap()
        .fail_appends_for_test(false)
        .unwrap();
    bob_node.clear_conversation(channel).unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    bob_node.retry_channel_plaintext();
    assert!(
        !bob_node
            .channel_history(channel, 10)
            .iter()
            .any(|entry| entry.id == erased)
    );
    assert!(
        std::iter::from_fn(|| b_ch_r.try_recv().ok())
            .all(|event| event.reception_only && event.text.is_empty()),
        "local erase must cancel queued user messages"
    );
    assert!(bob_node.channel_history(channel, 10).is_empty());
    bob_node
        .received
        .lock()
        .unwrap()
        .fail_appends_for_test(true)
        .unwrap();
    alice_node
        .send_channel_message(channel, b"prune before retry")
        .await
        .unwrap();
    bob_node
        .received
        .lock()
        .unwrap()
        .fail_appends_for_test(false)
        .unwrap();
    bob_node.prune_older_than(u64::MAX).unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    bob_node.retry_channel_plaintext();
    assert!(bob_node.channel_history(channel, 10).is_empty());
    assert!(
        std::iter::from_fn(|| b_ch_r.try_recv().ok())
            .all(|event| event.reception_only && event.text.is_empty()),
        "retention prune must cancel queued user messages"
    );
    recovery_task.abort();
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
    let empty_name = alice_node.rename_channel(channel, "   ").await.unwrap_err();
    assert!(
        empty_name.to_string().starts_with("invalid input:"),
        "local name rejection must be typed invalid input"
    );
    let mut info_changes = bob_node.subscribe_channel_changes();
    alice_node.rename_channel(channel, "renamed").await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), info_changes.recv())
            .await
            .expect("remote rename must invalidate group info without a chat message")
            .unwrap(),
        channel
    );
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

#[tokio::test]
async fn group_summary_and_presence_count_accounts_while_unknown_devices_remain_distinct() {
    let directory = tempfile::tempdir().unwrap();
    let roster = Arc::new(Mutex::new(Roster::default()));
    let (dm, _) = mpsc::unbounded_channel();
    let (channel_tx, _) = mpsc::unbounded_channel();
    let (files, _) = mpsc::unbounded_channel();
    let node = Node::open(
        DeviceIdentity::generate(),
        Arc::clone(&roster),
        dm,
        channel_tx,
        files,
        &directory.path().join("messages.log"),
        &directory.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let other_account = crate::identity::account::Account::generate();
    let siblings = [DeviceIdentity::generate(), DeviceIdentity::generate()];
    for (index, sibling) in siblings.iter().enumerate() {
        let proof = Announce::new_with_account(
            sibling,
            &other_account,
            "same person",
            47001 + index as u16,
        );
        roster
            .lock()
            .unwrap()
            .update(&proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &node.user_id());
    }
    let unknown = DeviceIdentity::generate();
    let channel = node
        .create_channel(
            "three people",
            vec![siblings[0].public(), siblings[1].public(), unknown.public()],
        )
        .await
        .unwrap();
    let (accounts, unbound) = node.channel_member_account_sets(channel);
    assert_eq!(accounts.len(), 2, "local account plus the remote account");
    assert_eq!(
        unbound.into_iter().collect::<Vec<_>>(),
        vec![unknown.public().user_id()]
    );
    assert_eq!(
        node.list_channels()
            .iter()
            .find(|row| row.id == channel)
            .unwrap()
            .member_count,
        3
    );
    let pending = node
        .create_channel_invited("one accepted person", vec![unknown.public()])
        .await
        .unwrap();
    assert_eq!(
        node.list_channels()
            .iter()
            .find(|row| row.id == pending)
            .unwrap()
            .member_count,
        1,
        "pending invitees are not accepted participants"
    );
}

#[tokio::test]
async fn owner_dissolves_channel_and_local_mutations_stop() {
    let me = DeviceIdentity::generate();
    let secrets = me.secret_bytes();
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
    let id = node.create_channel("general", vec![]).await.unwrap();
    node.send_channel_message(id, b"before closure")
        .await
        .unwrap();
    node.dissolve_channel(id).await.unwrap();
    assert!(node.list_channels()[0].dissolved);
    assert!(
        node.channel_history(id, 10)
            .iter()
            .any(|item| item.text == b"before closure")
    );
    assert!(matches!(
        node.send_channel_message(id, b"late").await,
        Err(NodeError::Channel(_))
    ));
    assert!(matches!(
        node.rename_channel(id, "late").await,
        Err(NodeError::Channel(_))
    ));
    assert!(matches!(
        node.dissolve_channel(id).await,
        Err(NodeError::Channel(_))
    ));
    drop(node);
    let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
    let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
    let (file_tx, _file_rx) = mpsc::unbounded_channel();
    let reopened = Node::open(
        DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
        Arc::new(Mutex::new(Roster::default())),
        dm_tx,
        ch_tx,
        file_tx,
        &dir.path().join("m.log"),
        &dir.path().join("m-sent.log"),
        "pw",
    )
    .unwrap();
    assert!(reopened.list_channels()[0].dissolved);
    assert!(
        reopened
            .channel_history(id, 10)
            .iter()
            .any(|item| item.text == b"before closure")
    );
    assert!(matches!(
        reopened.send_channel_message(id, b"late").await,
        Err(NodeError::Channel(_))
    ));
}

#[tokio::test]
async fn owner_dissolution_syncs_to_member_over_loopback() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let bob_public = bob.public();
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = listener.local_addr().unwrap();
    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice.public().user_id());
    let bob_roster = Arc::new(Mutex::new(Roster::default()));
    let (a_dm, _a_dm_rx) = mpsc::unbounded_channel();
    let (a_ch, _a_ch_rx) = mpsc::unbounded_channel();
    let (a_file, _a_file_rx) = mpsc::unbounded_channel();
    let (b_dm, _b_dm_rx) = mpsc::unbounded_channel();
    let (b_ch, _b_ch_rx) = mpsc::unbounded_channel();
    let (b_file, _b_file_rx) = mpsc::unbounded_channel();
    let owner = Node::open(
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
    let member = Node::open(
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
    tokio::spawn(Arc::clone(&member).run_accept_loop(listener));

    let channel = owner
        .create_channel("general", vec![bob_public])
        .await
        .unwrap();
    assert!(matches!(
        member.dissolve_channel(channel).await,
        Err(NodeError::Channel(_))
    ));
    owner.dissolve_channel(channel).await.unwrap();
    assert!(owner.list_channels()[0].dissolved);
    assert!(member.list_channels()[0].dissolved);
    assert!(matches!(
        member.send_channel_message(channel, b"late").await,
        Err(NodeError::Channel(_))
    ));
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
    let mut payload = vec![0x89u8; crate::file::CHUNK_SIZE + 4242];
    payload[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
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
    assert!(bob_node.log.lock().unwrap().events(&file_conv).is_empty());
    let progress = bob_node.file_progress(file_conv).unwrap();
    assert!(progress.total > 0 && progress.done == progress.total);
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
    bob_node.set_attachment_global_auto(true).unwrap();
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
    assert!(!rf.is_channel);

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

#[tokio::test]
async fn attachment_offer_waits_for_exact_recipient_approval() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let alice_roster = seed_roster(&bob, "Bob", port, &alice.public().user_id());
    let bob_roster = seed_roster(&alice, "Alice", 1, &bob.public().user_id());
    let dir = tempfile::tempdir().unwrap();
    let (a_dm, _) = mpsc::unbounded_channel();
    let (a_ch, _) = mpsc::unbounded_channel();
    let (a_file, _) = mpsc::unbounded_channel();
    let (b_dm, _) = mpsc::unbounded_channel();
    let (b_ch, _) = mpsc::unbounded_channel();
    let (b_file, mut received) = mpsc::unbounded_channel();
    let a = Node::open(
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
    let b = Node::open(
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
    tokio::spawn(Arc::clone(&b).run_accept_loop(listener));
    let payload = vec![0x53; crate::file::CHUNK_SIZE + 19];
    let source = dir.path().join("untrusted.bin");
    std::fs::write(&source, &payload).unwrap();
    let (card, file) = a
        .enqueue_file_dm(&b.user_id(), &source, crate::file::FileKind::File)
        .await
        .unwrap();
    let mut chunk_first = a
        .privacy_dial(
            SocketAddr::from(([127, 0, 0, 1], port)),
            &b.identity.public(),
        )
        .await
        .unwrap();
    let _ = crate::node::session::request_round(
        &mut chunk_first,
        &a.sync_store(&b.identity.public()),
        file,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(b.log.lock().unwrap().events(&file).is_empty());
    a.flush_file_delivery(card).await;
    let offer = tokio::time::timeout(Duration::from_secs(5), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(offer.file_conv, file);
    assert_eq!(
        b.attachment_consent_status(file),
        Some(crate::node::ReceiveDecision::Pending)
    );
    assert_eq!(b.file_progress(file).unwrap().done, 0);
    // Simulate an older sender bypassing the optional consent query. The
    // recipient's durable ingest gate still refuses unsolicited chunks.
    let mut unsolicited = a
        .privacy_dial(
            SocketAddr::from(([127, 0, 0, 1], port)),
            &b.identity.public(),
        )
        .await
        .unwrap();
    let _ = crate::node::session::request_round(
        &mut unsolicited,
        &a.sync_store(&b.identity.public()),
        file,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(b.file_progress(file).unwrap().done, 0);
    b.decide_attachment(file, false).unwrap();
    a.flush_file_delivery(card).await;
    assert_eq!(
        b.attachment_consent_status(file),
        Some(crate::node::ReceiveDecision::Declined)
    );
    assert_eq!(b.file_progress(file).unwrap().done, 0);
    b.decide_attachment(file, true).unwrap();
    assert!(
        !b.attachment_chunk_author_allowed(file, &DeviceIdentity::generate().public().ed25519_pub)
    );
    assert_eq!(
        b.attachment_consent_for_peer(
            file,
            crate::eventlog::EventId::new([0; 32]),
            &a.identity.public()
        ),
        crate::node::ReceiveDecision::Pending
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !b.file_ready_to_save(file) {
            a.flush_file_delivery(card).await;
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("chunks transfer after approval");
    let dest = dir.path().join("accepted.bin");
    b.save_file(file, &dest).unwrap();
    assert_eq!(std::fs::read(dest).unwrap(), payload);

    let image = dir.path().join("inline.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\nimage payload").unwrap();
    let image_file = a
        .send_file_dm(&b.user_id(), &image, crate::file::FileKind::Media)
        .await
        .unwrap();
    let image_offer = tokio::time::timeout(Duration::from_secs(5), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(image_offer.file_conv, image_file);
    assert_eq!(
        b.attachment_consent_status(image_file),
        Some(crate::node::ReceiveDecision::Accepted)
    );

    let video = dir.path().join("clip.mp4");
    std::fs::write(&video, b"video payload").unwrap();
    let video_file = a
        .send_file_dm(&b.user_id(), &video, crate::file::FileKind::Media)
        .await
        .unwrap();
    let video_offer = tokio::time::timeout(Duration::from_secs(5), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(video_offer.file_conv, video_file);
    assert_eq!(
        b.attachment_consent_status(video_file),
        Some(crate::node::ReceiveDecision::Pending)
    );
    assert_eq!(b.file_progress(video_file).unwrap().done, 0);
}

#[tokio::test]
async fn two_nodes_transfer_nested_directory_and_save_it() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_addr = listener.local_addr().unwrap();
    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice.public().user_id());
    let bob_roster = seed_roster(&alice, "Alice", 1, &bob.public().user_id());
    let (a_dm, _) = mpsc::unbounded_channel();
    let (a_ch, _) = mpsc::unbounded_channel();
    let (a_f, _) = mpsc::unbounded_channel();
    let (b_dm, _) = mpsc::unbounded_channel();
    let (b_ch, _) = mpsc::unbounded_channel();
    let (b_f, mut received) = mpsc::unbounded_channel();
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
    bob_node.set_attachment_global_auto(true).unwrap();
    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

    let source = dir.path().join("photos");
    std::fs::create_dir_all(source.join("nested/empty")).unwrap();
    std::fs::write(source.join("nested/pic.txt"), b"folder bytes").unwrap();
    let file_conv = alice_node
        .send_file_dm(&bob_node.user_id(), &source, crate::file::FileKind::Media)
        .await
        .unwrap();
    // Older clients use the unchanged file save path and get a usable tar archive.
    let legacy_tar = dir.path().join("legacy.tar");
    alice_node.save_file(file_conv, &legacy_tar).unwrap();
    let mut archive = tar::Archive::new(std::fs::File::open(&legacy_tar).unwrap());
    assert!(
        archive
            .entries()
            .unwrap()
            .any(|item| item.unwrap().path().unwrap() == std::path::Path::new("nested/pic.txt"))
    );
    let card = tokio::time::timeout(std::time::Duration::from_secs(5), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(card.name, "photos.tar");
    assert_eq!(card.mime, crate::file::directory::MIME);
    assert!(!card.media);
    let downloads = dir.path().join("downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    // Model extraction failing after archive verification: chunks remain available
    // so the user can retry the folder save instead of losing the transfer.
    let temporary_archive = downloads.join("attempt.tar");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match bob_node.save_directory_archive(card.file_conv, &temporary_archive) {
                Ok(()) => break,
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .expect("archive became complete");
    std::fs::remove_file(&temporary_archive).unwrap();
    assert!(bob_node.file_ready_to_save(card.file_conv));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match bob_node.save_file_into_dir(card.file_conv, &downloads) {
                Ok(path) => break path,
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .map(|path| {
        assert_eq!(path, downloads.join("photos"));
        assert_eq!(
            std::fs::read(path.join("nested/pic.txt")).unwrap(),
            b"folder bytes"
        );
        assert!(path.join("nested/empty").is_dir());
    })
    .expect("directory saved within 5s");
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
    bob_node.set_attachment_global_auto(true).unwrap();
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
    let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice.public().user_id());
    let bob_roster = seed_roster(
        &alice,
        "Alice",
        alice_listener.local_addr().unwrap().port(),
        &bob.public().user_id(),
    );

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
    bob_node.set_attachment_global_auto(true).unwrap();
    // ~5 chunks → the file_conv batch far exceeds one frame → multiple rounds.
    let payload = vec![0x5Au8; crate::file::CHUNK_SIZE * 4 + 999];
    let src = dir.path().join("big.bin");
    std::fs::write(&src, &payload).unwrap();

    let bob_uid = bob_node.user_id();
    let file_conv = alice_node
        .send_file_dm(&bob_uid, &src, crate::file::FileKind::File)
        .await
        .unwrap();

    // The initial best-effort dial has no accepting worker. Resume the durable
    // pending work rather than depending on it fitting the 400ms operation.
    assert_eq!(
        alice_node
            .delivery
            .lock()
            .unwrap()
            .next_file_destination(None)
            .unwrap()
            .0
            .file_conversation,
        file_conv
    );
    let original_chunks: Vec<_> = alice_node
        .log
        .lock()
        .unwrap()
        .events(&file_conv)
        .into_iter()
        .cloned()
        .collect();
    assert_eq!(original_chunks.len(), 5);
    let mut workers = tokio::task::JoinSet::new();
    workers.spawn(Arc::clone(&bob_node).run_accept_loop(listener));
    workers.spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));

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
    workers.shutdown().await;
    assert!(saved, "bob saved the multi-chunk file");
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
    assert_eq!(rf.file_conv, file_conv);
    assert_eq!(
        alice_node
            .log
            .lock()
            .unwrap()
            .events(&file_conv)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>(),
        original_chunks,
        "worker retries preserve the original signed chunks"
    );
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
    assert_eq!(
        bob_node.reactions(conv),
        bob_views,
        "cached reaction view is stable"
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
async fn stalled_relay_cannot_delay_inline_channel_photo_bytes_to_an_online_member() {
    let root = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let bob_public = bob.public();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let roster = seed_roster(
        &bob,
        "Bob",
        listener.local_addr().unwrap().port(),
        &alice.user_id(),
    );
    let bob_roster = seed_roster(&alice, "Alice", 1, &bob.user_id());
    let open = |name: &str, identity, roster| {
        let directory = root.path().join(name);
        std::fs::create_dir_all(&directory).unwrap();
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, _) = mpsc::unbounded_channel();
        let (files, received) = mpsc::unbounded_channel();
        let node = Node::open(
            identity,
            roster,
            dm,
            channel,
            files,
            &directory.join("messages.log"),
            &directory.join("sent.log"),
            "pw",
        )
        .unwrap();
        (node, received)
    };
    let (sender, _) = open("sender", alice, roster.clone());
    let (recipient, mut received) = open("recipient", bob, bob_roster);
    for (name, node, port) in [
        ("sender", &sender, 1),
        (
            "recipient",
            &recipient,
            listener.local_addr().unwrap().port(),
        ),
    ] {
        node.configure_privacy(
            &root.path().join(name),
            "pw",
            &node.signed_announce(name, port),
            std::sync::Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
    }
    let receiver = tokio::spawn(recipient.clone().run_accept_loop(listener));
    let channel = sender
        .create_channel("Photos", vec![bob_public])
        .await
        .unwrap();
    let relay = DeviceIdentity::generate();
    let relay_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    roster.lock().unwrap().update(
        &Announce::new_post_office(
            &relay,
            "Stalled relay",
            relay_listener.local_addr().unwrap().port(),
        ),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &sender.user_id(),
    );
    // Keep the TCP connection open without completing authentication. A live
    // member must get the bytes without waiting for this optional custody path.
    let relay_task = tokio::spawn(async move {
        let (_connection, _) = relay_listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let payload = b"\x89PNG\r\n\x1a\ninline photo payload";
    let path = root.path().join("photo.png");
    std::fs::write(&path, payload).unwrap();
    let sending = tokio::spawn(async move {
        sender
            .send_file_channel(channel, &path, crate::file::FileKind::Media)
            .await
    });
    let bytes = tokio::time::timeout(Duration::from_secs(3), async {
        let offer = received.recv().await.expect("file offer");
        loop {
            if let Some(bytes) = recipient.read_media(offer.file_conv) {
                break bytes;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    sending.abort();
    relay_task.abort();
    receiver.abort();
    assert_eq!(
        bytes.expect("an optional stalled relay must not hold back online photo bytes"),
        payload
    );
}

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
    assert!(rf.is_channel);
    assert_eq!(rf.file_conv, file_conv);
    assert_eq!(rf.from, alice_node.user_id());
    assert_eq!(bob_node.file_progress(file_conv).unwrap().done, 0);
    bob_node.decide_attachment(file_conv, true).unwrap();

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
    assert_eq!(alice_node.delivery_status(alice_file.id), None);
    assert_eq!(bob_node.delivery_status(bob_file.id), None);
    assert!(
        bob_node
            .delivery
            .lock()
            .unwrap()
            .retry_receipts(1)
            .is_empty(),
        "channel file cards must not generate DM delivery receipts"
    );

    let dest = dir.path().join("saved.bin");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            bob_node.pull_pending_files().await;
            match bob_node.save_file(rf.file_conv, &dest) {
                Ok(()) => break,
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .expect("bob saved the channel file within 5s");
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
    assert_eq!(
        bob_node.log.lock().unwrap().events(&file_conv).len(),
        rf.size.div_ceil(crate::file::CHUNK_SIZE as u64) as usize,
        "a saved channel file stays available as a bounded peer replica"
    );

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
    assert_eq!(alice_node.file_progress(file_conv2).unwrap().done, 0);
    alice_node.decide_attachment(file_conv2, true).unwrap();

    let dest2 = dir.path().join("alice-saved.bin");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            alice_node.pull_pending_files().await;
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

#[tokio::test]
async fn saved_channel_file_remains_downloadable_from_member_after_author_disconnects() {
    // Drive recovery explicitly below so background anti-entropy cannot win
    // the initial live push or fetch the deliberately missed second manifest.
    // Use the same authenticated serving path as the controlled group rigs.
    fn serve(node: Arc<Node>, listener: TcpListener) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let node = Arc::clone(&node);
                connections.spawn(async move {
                    if let Ok(channel) = node.privacy_accept(stream).await {
                        node.serve_connection(channel).await;
                    }
                });
                while connections.try_join_next().is_some() {}
            }
        })
    }
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let carol = DeviceIdentity::generate();
    let alice_id = alice.public().user_id();
    let carol_id = carol.public().user_id();
    let dir = tempfile::tempdir().unwrap();
    let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let alice_port = alice_listener.local_addr().unwrap().port();
    let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bob_port = bob_listener.local_addr().unwrap().port();
    let alice_roster = seed_roster(&bob, "Bob", bob_port, &alice_id);
    let bob_roster = seed_roster(&alice, "Alice", alice_port, &bob.public().user_id());
    let carol_roster = seed_roster(&alice, "Alice", alice_port, &carol_id);
    let open = |identity, roster, name: &str| {
        let path = dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
        let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
        let (file_tx, file_rx) = mpsc::unbounded_channel();
        let node = Node::open(
            identity,
            roster,
            dm_tx,
            ch_tx,
            file_tx,
            &path.join("messages.log"),
            &path.join("sent.log"),
            "pw",
        )
        .unwrap();
        (node, file_rx)
    };
    let (alice_node, _alice_rx) = open(alice, alice_roster, "alice");
    let (bob_node, mut bob_rx) = open(bob, bob_roster, "bob");
    let (carol_node, mut carol_rx) = open(carol, carol_roster, "carol");
    let alice_task = serve(Arc::clone(&alice_node), alice_listener);
    let bob_task = serve(Arc::clone(&bob_node), bob_listener);
    let carol_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let carol_port = carol_listener.local_addr().unwrap().port();
    let carol_task = serve(Arc::clone(&carol_node), carol_listener);
    add_peer(
        &alice_node.roster,
        &carol_node.identity,
        "Carol",
        carol_port,
        &alice_id,
    );
    let channel = alice_node
        .create_channel(
            "files",
            vec![bob_node.identity.public(), carol_node.identity.public()],
        )
        .await
        .unwrap();
    let payload = vec![0xA5; crate::file::CHUNK_SIZE + 17];
    let source = dir.path().join("source.bin");
    std::fs::write(&source, &payload).unwrap();
    let file = alice_node
        .send_file_channel(channel, &source, crate::file::FileKind::File)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), bob_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let live_card = tokio::time::timeout(Duration::from_secs(5), carol_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(live_card.arrival_kind, "live");
    assert!(live_card.received_at > 0);
    assert!(!live_card.own_device);
    assert!(live_card.batch_complete);
    assert!(
        carol_node.channel_history(channel, 10).iter().any(
            |entry| entry.id == live_card.event_id && entry.wall_clock == live_card.authored_at
        )
    );
    bob_node.decide_attachment(file, true).unwrap();
    let bob_dest = dir.path().join("bob.bin");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            bob_node.pull_pending_files().await;
            if bob_node.save_file(file, &bob_dest).is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read(&bob_dest).unwrap(), payload);
    assert_eq!(bob_node.log.lock().unwrap().events(&file).len(), 2);

    // Carol misses a later card, then learns it through Bob's retained group log.
    alice_node
        .roster
        .lock()
        .unwrap()
        .forget(&carol_node.identity.public());
    let missed_file = alice_node
        .send_file_channel(channel, &source, crate::file::FileKind::File)
        .await
        .unwrap();
    assert!(carol_rx.try_recv().is_err());
    alice_task.abort();
    alice_task.await.unwrap_err();
    bob_node
        .roster
        .lock()
        .unwrap()
        .forget(&alice_node.identity.public());
    carol_node
        .roster
        .lock()
        .unwrap()
        .forget(&alice_node.identity.public());
    add_peer(
        &carol_node.roster,
        &bob_node.identity,
        "Bob",
        bob_port,
        &carol_id,
    );
    carol_node.pull_one_channel_from_member(0).await;
    let recovered_card = tokio::time::timeout(Duration::from_secs(5), carol_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered_card.file_conv, missed_file);
    assert_eq!(recovered_card.arrival_kind, "catch_up");
    assert!(
        recovered_card
            .batch_id
            .as_deref()
            .unwrap()
            .starts_with("file:")
    );
    assert!(recovered_card.batch_complete);
    assert_eq!(
        carol_node.file_progress(missed_file).unwrap().done,
        0,
        "card reception is distinct from byte availability"
    );
    carol_node.process_file_events_reception(channel, "catch_up");
    assert!(
        carol_rx.try_recv().is_err(),
        "duplicate recovered file cards stay silent"
    );
    carol_node.decide_attachment(file, true).unwrap();
    bob_node
        .set_channel_replica_enabled(channel, false)
        .await
        .unwrap();
    carol_node.pull_pending_files().await;
    assert_eq!(carol_node.file_progress(file).unwrap().done, 0);
    bob_node
        .set_channel_replica_enabled(channel, true)
        .await
        .unwrap();
    let carol_dest = dir.path().join("carol.bin");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            carol_node.pull_pending_files().await;
            if carol_node.save_file(file, &carol_dest).is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Carol downloaded from Bob after Alice disconnected");
    assert_eq!(std::fs::read(&carol_dest).unwrap(), payload);
    bob_task.abort();
    carol_task.abort();
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
    let before = bob_node.log.lock().unwrap().events(&channel).len();
    assert!(
        bob_node
            .react_channel_if(channel, target, "blocked", false, |_| Err(
                NodeError::Authorization("session changed".into())
            ))
            .await
            .is_err()
    );
    assert_eq!(
        bob_node.log.lock().unwrap().events(&channel).len(),
        before,
        "denied channel reaction has no signed event"
    );
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
    let mut recall_persisted = false;
    for _ in 0..50 {
        if alice_node
            .recalls
            .lock()
            .unwrap()
            .targets(&channel)
            .contains(&bob_msg_id)
        {
            recall_persisted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        recall_persisted,
        "peer channel recall persisted before backup"
    );
    let mut snapshot = Vec::new();
    alice_node
        .write_recovery_snapshot(&mut snapshot, 100)
        .unwrap();
    let restored = Node::install_recovery_snapshot(
        std::io::Cursor::new(snapshot),
        &dir.path().join("recovered-alice"),
        "new password",
    )
    .unwrap();
    assert!(
        restored
            .channel_history(channel, 20)
            .iter()
            .any(|entry| entry.id == bob_msg_id && entry.recalled),
        "an unseen peer channel recall must survive recovery"
    );

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
    let dm_conv = dm_conversation_id(&alice_node.identity.public(), &bob_node.identity.public());
    let mut delete_arrived = false;
    for _ in 0..50 {
        if alice_node
            .log
            .lock()
            .unwrap()
            .events(&dm_conv)
            .iter()
            .any(|event| event.kind == crate::eventlog::EventKind::Delete)
        {
            delete_arrived = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        delete_arrived,
        "alice received the signed recall before backup"
    );
    let mut snapshot = Vec::new();
    alice_node
        .write_recovery_snapshot(&mut snapshot, 100)
        .unwrap();
    let restored = Node::install_recovery_snapshot(
        std::io::Cursor::new(snapshot),
        &dir.path().join("recovered-alice"),
        "new password",
    )
    .unwrap();
    assert!(
        restored
            .account_history(&bob_acct.account_id(), 10)
            .iter()
            .any(|entry| entry.id == bob_msg_id && entry.recalled),
        "an unseen verified peer recall must survive recovery without old device keys"
    );
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

fn personal_message_node(
    dir: &std::path::Path,
    identity: DeviceIdentity,
    account: crate::identity::account::Account,
) -> Arc<Node> {
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    Node::open_with_account(
        identity,
        account,
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        file,
        &dir.join("events.log"),
        &dir.join("sent.log"),
        "pw",
    )
    .unwrap()
}

#[tokio::test]
async fn personal_message_caches_new_sibling_binding_before_enqueue() {
    use crate::{discovery::DiscoveryVisibility, identity::account::Account};
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let target = account.account_id();
    let sibling = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&sibling, &account, "Sibling", 1);
    let node = personal_message_node(dir.path(), DeviceIdentity::generate(), account);
    node.configure_privacy(
        dir.path(),
        "pw",
        &node.signed_announce("Own device", 1),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    // Discovery is visible before the host's next durable peer-snapshot query.
    node.roster.lock().unwrap().update(
        &proof,
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        &node.user_id(),
    );
    assert!(
        node.historical_author(&sibling.public().ed25519_pub)
            .is_none()
    );
    let id = node
        .enqueue_to_account(&target, b"new sibling note", None)
        .await
        .unwrap();
    assert_eq!(node.account_history(&target, 10)[0].id, id);
    assert!(node.account_delivery_statuses(&target, &[id]).is_empty());
    let stored = node.delivery.lock().unwrap();
    let destination = &stored.message(id).unwrap().destinations[0];
    assert_eq!(destination.account.as_deref(), Some(target.as_str()));
    assert!(!destination.receipt_eligible);
}

#[tokio::test]
async fn personal_message_is_durable_without_siblings() {
    use crate::identity::account::Account;
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let secret = identity.secret_bytes();
    let account = Account::generate();
    let account_secret = account.secret_bytes();
    let target = account.account_id();
    let node = personal_message_node(dir.path(), identity, account);
    assert!(node.account_fanout_targets(&target).is_empty());
    let id = node
        .enqueue_to_account(&target, b"private note", None)
        .await
        .unwrap();
    let history = node.account_history(&target, 10);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, id);
    assert!(history[0].from_me);
    assert_eq!(history[0].text, b"private note");
    assert!(node.log.lock().unwrap().conversations().is_empty());
    assert!(node.delivery.lock().unwrap().message(id).is_none());
    assert_eq!(node.delivery_status(id), None);
    drop(node);
    let node = personal_message_node(
        dir.path(),
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Account::from_secret_bytes(account_secret),
    );
    let history = node.account_history(&target, 10);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, id);
    assert!(history[0].from_me);
    assert_eq!(history[0].text, b"private note");
    assert!(
        node.delivery
            .lock()
            .unwrap()
            .pending_transactions()
            .is_empty()
    );
}

#[tokio::test]
async fn personal_message_unavailable_storage_is_unaccepted() {
    use crate::identity::account::Account;
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    let identity = DeviceIdentity::generate();
    let secret = identity.secret_bytes();
    let account = Account::generate();
    let account_secret = account.secret_bytes();
    let target = account.account_id();
    let node = personal_message_node(&profile, identity, account);
    node.delivery
        .lock()
        .unwrap()
        .fail_journal_appends_for_test(true)
        .unwrap();
    let result = node
        .enqueue_to_account(&target, b"not accepted", None)
        .await;
    node.delivery
        .lock()
        .unwrap()
        .fail_journal_appends_for_test(false)
        .unwrap();
    assert!(matches!(result, Err(NodeError::Log(_))));
    assert!(
        node.delivery
            .lock()
            .unwrap()
            .pending_transactions()
            .is_empty()
    );
    assert!(node.account_history(&target, 10).is_empty());
    drop(node);
    let node = personal_message_node(
        &profile,
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Account::from_secret_bytes(account_secret),
    );
    assert!(node.account_history(&target, 10).is_empty());
    assert!(
        node.delivery
            .lock()
            .unwrap()
            .pending_transactions()
            .is_empty()
    );
}

#[tokio::test]
async fn personal_message_projection_failure_recovers_exactly_once() {
    use crate::identity::account::Account;
    for reopen in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let secret = identity.secret_bytes();
        let account = Account::generate();
        let account_secret = account.secret_bytes();
        let target = account.account_id();
        let node = personal_message_node(dir.path(), identity, account);
        node.sentlog
            .lock()
            .unwrap()
            .fail_appends_for_test(true)
            .unwrap();
        let id = node
            .enqueue_to_account(&target, b"recover note", None)
            .await
            .unwrap();
        assert_eq!(
            node.delivery.lock().unwrap().pending_transactions().len(),
            1
        );
        assert_eq!(node.account_history(&target, 10).len(), 1);
        assert_eq!(node.account_history(&target, 10)[0].id, id);
        node.sentlog
            .lock()
            .unwrap()
            .fail_appends_for_test(false)
            .unwrap();
        let node = if reopen {
            drop(node);
            personal_message_node(
                dir.path(),
                DeviceIdentity::from_secret_bytes(secret.0, secret.1),
                Account::from_secret_bytes(account_secret),
            )
        } else {
            node.recover_delivery(&mut node.delivery.lock().unwrap())
                .unwrap();
            node
        };
        node.recover_delivery(&mut node.delivery.lock().unwrap())
            .unwrap();
        let history = node.account_history(&target, 10);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, id);
        assert!(history[0].from_me);
        assert_eq!(history[0].text, b"recover note");
        assert!(
            node.delivery
                .lock()
                .unwrap()
                .pending_transactions()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn personal_message_later_linked_sibling_catches_up_once() {
    use crate::identity::account::Account;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let account_secret = account.secret_bytes();
    let target = account.account_id();
    let source = personal_message_node(
        &dir.path().join("source"),
        DeviceIdentity::generate(),
        account,
    );
    let id = source
        .enqueue_to_account(&target, b"before linking", None)
        .await
        .unwrap();
    let sibling = personal_message_node(
        &dir.path().join("sibling"),
        DeviceIdentity::generate(),
        Account::from_secret_bytes(account_secret),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    for (owner, other) in [(&source, &sibling), (&sibling, &source)] {
        owner
            .account_devices
            .lock()
            .unwrap()
            .insert(crate::node::account_devices::AccountDevice {
                public: other.identity.public(),
                certificate: owner.account.certify(&other.identity.public().ed25519_pub),
            })
            .unwrap();
    }
    add_account_peer(
        &sibling.roster,
        &source.identity,
        &source.account,
        "source",
        listener.local_addr().unwrap().port(),
        &sibling.user_id(),
    );
    let server = tokio::spawn(source.clone().run_accept_loop(listener));
    // Restart the pull worker to perform two complete sweeps with fresh cursors.
    // Both must merge the same logical message rather than duplicating history.
    for _ in 0..2 {
        sibling.account_sync_status.lock().unwrap().clear();
        let worker = tokio::spawn(sibling.clone().run_account_history_loop());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if sibling
                    .device_sync_status()
                    .iter()
                    .any(|status| status.phase == SyncPhase::UpToDate)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        worker.abort();
        let _ = worker.await;
    }
    let history = sibling.account_history(&target, 10);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, id);
    assert!(history[0].from_me);
    assert_eq!(history[0].text, b"before linking");
    server.abort();
    let _ = server.await;
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
async fn linked_device_catches_up_incoming_dm_after_reconnect() {
    use crate::identity::account::Account;

    let a1 = DeviceIdentity::generate();
    let a2 = DeviceIdentity::generate();
    let alice_account = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let dir = tempfile::tempdir().unwrap();
    // Each simulated device needs its own private sidecars and media key.
    let a1_store = dir.path().join("a1");
    let a2_store = dir.path().join("a2");
    let bob_store = dir.path().join("bob");
    for store in [&a1_store, &a2_store, &bob_store] {
        std::fs::create_dir_all(store).unwrap();
    }

    let a1_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a1_port = a1_listener.local_addr().unwrap().port();
    let a2_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a2_port = a2_listener.local_addr().unwrap().port();

    let a1_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(
        &a1_roster,
        &a2,
        &alice_account,
        "A2",
        a2_port,
        &a1.public().user_id(),
    );
    add_account_peer(
        &a1_roster,
        &bob,
        &bob_account,
        "Bob",
        4000,
        &a1.public().user_id(),
    );
    let a2_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(
        &a2_roster,
        &a1,
        &alice_account,
        "A1",
        a1_port,
        &a2.public().user_id(),
    );
    let bob_roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(
        &bob_roster,
        &a1,
        &alice_account,
        "A1",
        a1_port,
        &bob.public().user_id(),
    );

    let (a1_dm, _a1_dm_rx) = mpsc::unbounded_channel();
    let (a1_ch, _a1_ch_rx) = mpsc::unbounded_channel();
    let (a1_file, _a1_file_rx) = mpsc::unbounded_channel();
    let (a2_dm, _a2_dm_rx) = mpsc::unbounded_channel();
    let (a2_ch, _a2_ch_rx) = mpsc::unbounded_channel();
    let (a2_file, _a2_file_rx) = mpsc::unbounded_channel();
    let (bob_dm, _bob_dm_rx) = mpsc::unbounded_channel();
    let (bob_ch, _bob_ch_rx) = mpsc::unbounded_channel();
    let (bob_file, _bob_file_rx) = mpsc::unbounded_channel();

    let a1_node = Node::open_with_account(
        a1,
        Account::from_secret_bytes(alice_account.secret_bytes()),
        a1_roster,
        a1_dm,
        a1_ch,
        a1_file,
        &a1_store.join("events.log"),
        &a1_store.join("sent.log"),
        "pw",
    )
    .unwrap();
    let a2_node = Node::open_with_account(
        a2,
        Account::from_secret_bytes(alice_account.secret_bytes()),
        a2_roster,
        a2_dm,
        a2_ch,
        a2_file,
        &a2_store.join("events.log"),
        &a2_store.join("sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open_with_account(
        bob,
        Account::from_secret_bytes(bob_account.secret_bytes()),
        bob_roster,
        bob_dm,
        bob_ch,
        bob_file,
        &bob_store.join("events.log"),
        &bob_store.join("sent.log"),
        "pw",
    )
    .unwrap();

    // These certificates are recorded by pairing in a real runtime. This test starts
    // with an already-linked account and seeds the same trusted sibling directory.
    a1_node
        .account_devices
        .lock()
        .unwrap()
        .insert(crate::node::account_devices::AccountDevice {
            public: a2_node.identity.public(),
            certificate: alice_account.certify(&a2_node.identity.public().ed25519_pub),
        })
        .unwrap();
    a2_node
        .account_devices
        .lock()
        .unwrap()
        .insert(crate::node::account_devices::AccountDevice {
            public: a1_node.identity.public(),
            certificate: alice_account.certify(&a1_node.identity.public().ed25519_pub),
        })
        .unwrap();
    let a3 = DeviceIdentity::generate();
    a1_node
        .account_devices
        .lock()
        .unwrap()
        .insert(crate::node::account_devices::AccountDevice {
            public: a3.public(),
            certificate: alice_account.certify(&a3.public().ed25519_pub),
        })
        .unwrap();
    a2_node
        .set_device_sync_category(crate::node::SyncCategory::History, false)
        .unwrap();

    let channel = a1_node.create_channel("old channel", vec![]).await.unwrap();
    a1_node
        .send_channel_message(channel, b"before sibling joined")
        .await
        .unwrap();
    let old_channel_id = a1_node.channel_history(channel, 10)[0].id;
    let channel_attachment_bytes = vec![0x35u8; 24_000];
    let channel_attachment_path = dir.path().join("old-channel.bin");
    std::fs::write(&channel_attachment_path, &channel_attachment_bytes).unwrap();
    let channel_attachment_file = a1_node
        .send_file_channel(
            channel,
            &channel_attachment_path,
            crate::file::FileKind::File,
        )
        .await
        .unwrap();
    let channel_attachment_id = a1_node
        .channel_history(channel, 10)
        .into_iter()
        .find(|entry| {
            entry
                .file
                .as_ref()
                .is_some_and(|file| file.file_conv == channel_attachment_file)
        })
        .unwrap()
        .id;
    let attachment_bytes = vec![0x42u8; 90_000];
    let attachment_path = dir.path().join("before-link.bin");
    std::fs::write(&attachment_path, &attachment_bytes).unwrap();
    let (attachment_id, attachment_file) = a1_node
        .enqueue_file_to_account(
            &bob_account.account_id(),
            &attachment_path,
            crate::file::FileKind::File,
        )
        .await
        .unwrap();
    let dm_texts = |node: &Arc<Node>| {
        node.account_history(&bob_account.account_id(), 10)
            .into_iter()
            .filter(|entry| entry.file.is_none())
            .collect::<Vec<_>>()
    };
    let channel_texts = |node: &Arc<Node>| {
        node.channel_history(channel, 10)
            .into_iter()
            .filter(|entry| entry.file.is_none())
            .collect::<Vec<_>>()
    };

    let a1_task = tokio::spawn(Arc::clone(&a1_node).run_accept_loop(a1_listener));
    bob_node
        .send_to_account(&alice_account.account_id(), b"while A2 was offline", None)
        .await
        .unwrap();
    for _ in 0..50 {
        if dm_texts(&a1_node).len() == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(dm_texts(&a1_node).len(), 1);
    assert!(dm_texts(&a2_node).is_empty());

    let a2_task = tokio::spawn(Arc::clone(&a2_node).run_accept_loop(a2_listener));
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if a2_node
                .list_channels()
                .iter()
                .any(|item| item.id == channel && !item.pending_invite)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("certified sibling joins the existing channel");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(channel_texts(&a2_node).is_empty());
    assert!(
        dm_texts(&a2_node).is_empty(),
        "disabled history sync must not import sibling messages"
    );
    a2_node
        .set_device_sync_category(crate::node::SyncCategory::History, true)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if channel_texts(&a2_node)
                .iter()
                .any(|item| item.id == old_channel_id)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("old channel plaintext reaches the linked device");
    let archived = channel_texts(&a2_node);
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].id, old_channel_id);
    assert_eq!(archived[0].text, b"before sibling joined");
    assert!(archived[0].from_me, "another own device is still 'you'");
    assert_eq!(
        a2_node
            .delete_message(channel, old_channel_id, false)
            .unwrap(),
        1
    );
    a1_node
        .send_channel_message(channel, b"after sibling joined")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if channel_texts(&a2_node)
                .iter()
                .any(|item| item.text == b"after sibling joined")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("new channel messages still arrive");
    tokio::time::sleep(Duration::from_secs(11)).await;
    let history = channel_texts(&a2_node);
    assert_eq!(
        history.len(),
        1,
        "sibling history must not resurrect a local erase"
    );
    assert_eq!(history[0].text, b"after sibling joined");
    for _ in 0..50 {
        if dm_texts(&a2_node).len() == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let history = dm_texts(&a2_node);
    assert_eq!(history.len(), 1, "A2 must recover the DM from A1");
    assert_eq!(history[0].text, b"while A2 was offline");
    assert!(!history[0].from_me);
    assert!(
        a2_node
            .linked_devices()
            .iter()
            .any(|device| device.public == a3.public()),
        "A2 must learn A3's certified key through A1"
    );

    a1_node
        .send_to_account(&bob_account.account_id(), b"from A1 to Bob", None)
        .await
        .unwrap();
    for _ in 0..50 {
        if dm_texts(&a2_node).len() == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let history = dm_texts(&a2_node);
    assert_eq!(history.len(), 2, "A2 must recover A1's outgoing DM once");
    assert_eq!(history[1].text, b"from A1 to Bob");
    assert!(history[1].from_me);

    a1_node
        .set_device_sync_category(crate::node::SyncCategory::Appearance, true)
        .unwrap();
    a2_node
        .set_device_sync_category(crate::node::SyncCategory::Appearance, true)
        .unwrap();
    a1_node
        .set_account_preference(
            crate::node::SyncCategory::Appearance,
            "theme".into(),
            Some("light".into()),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if a2_node
                .account_preferences(crate::node::SyncCategory::Appearance)
                .iter()
                .any(|entry| entry.key == "theme" && entry.value.as_deref() == Some("light"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("appearance preference reaches certified sibling");
    a2_node
        .set_device_sync_category(crate::node::SyncCategory::Appearance, false)
        .unwrap();
    a1_node
        .set_account_preference(
            crate::node::SyncCategory::Appearance,
            "theme".into(),
            Some("dark".into()),
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        a2_node.account_preferences(crate::node::SyncCategory::Appearance)[0]
            .value
            .as_deref(),
        Some("light")
    );
    a2_node
        .set_device_sync_category(crate::node::SyncCategory::Appearance, true)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if a2_node
                .account_preferences(crate::node::SyncCategory::Appearance)
                .iter()
                .any(|entry| entry.key == "theme" && entry.value.as_deref() == Some("dark"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("re-enabling appearance catches up");

    a1_node
        .set_device_sync_category(crate::node::SyncCategory::Contacts, true)
        .unwrap();
    a2_node
        .set_device_sync_category(crate::node::SyncCategory::Contacts, true)
        .unwrap();
    let contact_key = format!("favorite:{}", bob_account.account_id());
    a1_node
        .set_account_preference(
            crate::node::SyncCategory::Contacts,
            contact_key.clone(),
            Some(r#"{"pinned":true,"custom_alias":"Bob","hidden":false}"#.into()),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if a2_node
                .account_preferences(crate::node::SyncCategory::Contacts)
                .iter()
                .any(|entry| {
                    entry.key == contact_key
                        && entry
                            .value
                            .as_deref()
                            .is_some_and(|value| value.contains("Bob"))
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("contact preference reaches certified sibling");

    a1_node
        .set_device_sync_category(crate::node::SyncCategory::Attachments, true)
        .unwrap();
    a2_node
        .set_device_sync_category(crate::node::SyncCategory::Attachments, true)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(35), async {
        loop {
            if a2_node
                .account_history(&bob_account.account_id(), 20)
                .iter()
                .any(|entry| entry.id == attachment_id && entry.file.is_some())
                && a2_node.file_ready_to_save(attachment_file)
                && a2_node.media.contains(attachment_file)
                && a2_node
                    .channel_history(channel, 20)
                    .iter()
                    .any(|entry| entry.id == channel_attachment_id && entry.file.is_some())
                && a2_node.file_ready_to_save(channel_attachment_file)
                && a2_node.media.contains(channel_attachment_file)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "old attachment card and bytes reach certified sibling: status={:?}, account_card={}, channel_card={}, account_ready={}, channel_ready={}",
            a2_node.device_sync_status(),
            a2_node.account_history(&bob_account.account_id(), 20).iter().any(|entry| entry.id == attachment_id && entry.file.is_some()),
            a2_node.channel_history(channel, 20).iter().any(|entry| entry.id == channel_attachment_id && entry.file.is_some()),
            a2_node.file_ready_to_save(attachment_file),
            a2_node.file_ready_to_save(channel_attachment_file),
        )
    });
    assert!(
        a2_node
            .account_file_proofs
            .lock()
            .unwrap()
            .get(&attachment_id)
            .is_some()
    );
    assert!(
        a2_node
            .account_file_proofs
            .lock()
            .unwrap()
            .get(&channel_attachment_id)
            .is_some()
    );
    let saved = dir.path().join("sibling-copy.bin");
    a2_node.save_file(attachment_file, &saved).unwrap();
    assert_eq!(std::fs::read(saved).unwrap(), attachment_bytes);
    let channel_saved = dir.path().join("sibling-channel-copy.bin");
    a2_node
        .save_file(channel_attachment_file, &channel_saved)
        .unwrap();
    assert_eq!(
        std::fs::read(channel_saved).unwrap(),
        channel_attachment_bytes
    );

    let (ed25519_secret, x25519_secret) = a2_node.identity.secret_bytes();
    a2_task.abort();
    let _ = a2_task.await;
    drop(a2_node);
    let (reopen_dm, _reopen_dm_rx) = mpsc::unbounded_channel();
    let (reopen_ch, _reopen_ch_rx) = mpsc::unbounded_channel();
    let (reopen_file, _reopen_file_rx) = mpsc::unbounded_channel();
    let reopened = Node::open_with_account(
        DeviceIdentity::from_secret_bytes(ed25519_secret, x25519_secret),
        Account::from_secret_bytes(alice_account.secret_bytes()),
        Arc::new(Mutex::new(Roster::default())),
        reopen_dm,
        reopen_ch,
        reopen_file,
        &a2_store.join("events.log"),
        &a2_store.join("sent.log"),
        "pw",
    )
    .unwrap();
    assert!(
        reopened
            .account_history(&bob_account.account_id(), 20)
            .iter()
            .any(|entry| entry.id == attachment_id && entry.file.is_some())
    );
    let ready = reopened.file_ready_to_save(attachment_file);
    let progress = reopened.file_progress(attachment_file);
    let (has_manifest, manifest_events) = {
        let files = reopened.files.lock().unwrap();
        (
            files.manifest(&attachment_file).is_some(),
            files.manifest_events(attachment_file).len(),
        )
    };
    let media_exists = reopened
        .media
        .path_for(attachment_file, "before-link.bin")
        .exists();
    let has_proof = reopened
        .account_file_proofs
        .lock()
        .unwrap()
        .get(&attachment_id)
        .is_some();
    assert!(
        ready,
        "account file after reopen: progress={progress:?} manifest={has_manifest} media_exists={media_exists} manifest_events={manifest_events} proof={has_proof}"
    );
    assert!(reopened.file_ready_to_save(channel_attachment_file));
    let reopened_account_copy = dir.path().join("reopened-sibling-copy.bin");
    reopened
        .save_file(attachment_file, &reopened_account_copy)
        .unwrap();
    assert_eq!(
        std::fs::read(reopened_account_copy).unwrap(),
        attachment_bytes
    );
    let reopened_channel_copy = dir.path().join("reopened-channel-copy.bin");
    reopened
        .save_file(channel_attachment_file, &reopened_channel_copy)
        .unwrap();
    assert_eq!(
        std::fs::read(reopened_channel_copy).unwrap(),
        channel_attachment_bytes
    );
    a1_task.abort();
}

#[test]
fn account_history_deduplicates_direct_and_linked_copies_by_message_id() {
    use crate::identity::account::Account;
    use crate::node::conversation::account_conversation_id;
    use crate::node::dm_envelope::DmEnvelope;
    use crate::node::message::MessageBody;
    use crate::node::pairing::BackfillRecord;

    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let bob = Account::generate();
    let own_id = account.account_id();
    let bob_id = bob.account_id();
    let conv = account_conversation_id(&own_id, &bob_id);
    let (dm, _dm_rx) = mpsc::unbounded_channel();
    let (ch, _ch_rx) = mpsc::unbounded_channel();
    let (file, _file_rx) = mpsc::unbounded_channel();
    let node = Node::open_with_account(
        DeviceIdentity::generate(),
        account,
        Arc::new(Mutex::new(Roster::default())),
        dm,
        ch,
        file,
        &dir.path().join("messages.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    node.set_device_sync_category(crate::node::SyncCategory::Attachments, true)
        .unwrap();
    assert!(node.device_sync_policy().attachments);
    let msg_id = [7; 32];
    let plaintext = DmEnvelope::new(
        bob_id.clone(),
        own_id,
        msg_id,
        MessageBody::new(b"one logical DM".to_vec(), None).encode(),
    )
    .encode();
    node.received
        .lock()
        .unwrap()
        .record(conv, bob_id.clone(), 1, &plaintext, EventId::new([8; 32]))
        .unwrap();
    let record = BackfillRecord {
        conv: *conv.as_bytes(),
        from: bob_id.clone(),
        wall_clock: 1,
        plaintext,
        event_id: msg_id,
    };
    node.import_account_backfill(std::slice::from_ref(&record))
        .unwrap();
    assert_eq!(node.account_history(&bob_id, 10).len(), 1);
    assert_eq!(node.received.lock().unwrap().entries(&conv).len(), 1);
    let previous_account = Account::generate();
    let previous_conv = account_conversation_id(&previous_account.account_id(), &bob_id);
    let previous_plaintext = DmEnvelope::new(
        bob_id.clone(),
        previous_account.account_id(),
        [19; 32],
        MessageBody::new(b"previous identity".to_vec(), None).encode(),
    )
    .encode();
    node.received
        .lock()
        .unwrap()
        .record(
            previous_conv,
            bob_id.clone(),
            1,
            &previous_plaintext,
            EventId::new([20; 32]),
        )
        .unwrap();
    assert_eq!(node.export_account_backfill().len(), 1);
    let mut invalid = record.clone();
    invalid.conv = *previous_conv.as_bytes();
    node.import_account_backfill(&[invalid]).unwrap();
    assert!(node.received.lock().unwrap().entries(&previous_conv).len() == 1);
    node.delete_account_message(&bob_id, EventId::new(msg_id))
        .unwrap();
    node.import_account_backfill(&[record]).unwrap();
    assert!(node.account_history(&bob_id, 10).is_empty());
}

#[test]
fn linked_history_clear_and_retention_survive_restart_without_resurrection() {
    use crate::identity::account::Account;
    use crate::node::conversation::account_conversation_id;
    use crate::node::dm_envelope::DmEnvelope;
    use crate::node::message::MessageBody;
    use crate::node::pairing::BackfillRecord;

    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate().secret_bytes();
    let account = Account::generate().secret_bytes();
    let own = Account::from_secret_bytes(account).account_id();
    let peer = Account::generate().account_id();
    let conversation = account_conversation_id(&own, &peer);
    let open = || {
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, _) = mpsc::unbounded_channel();
        let (file, _) = mpsc::unbounded_channel();
        Node::open_with_account(
            DeviceIdentity::from_secret_bytes(identity.0, identity.1),
            Account::from_secret_bytes(account),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            &dir.path().join("node.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap()
    };
    let record = |id: u8, wall_clock: u64| BackfillRecord {
        conv: *conversation.as_bytes(),
        from: peer.clone(),
        wall_clock,
        plaintext: DmEnvelope::new(
            peer.clone(),
            own.clone(),
            [id; 32],
            MessageBody::new(format!("message {id}").into_bytes(), None).encode(),
        )
        .encode(),
        event_id: [id; 32],
    };

    let cleared = record(31, 10);
    let node = open();
    node.import_account_backfill(std::slice::from_ref(&cleared))
        .unwrap();
    assert_eq!(node.clear_account_conversation(&peer).unwrap(), 1);
    node.set_device_sync_category(crate::node::SyncCategory::History, false)
        .unwrap();
    drop(node);

    let node = open();
    assert!(!node.device_sync_policy().history);
    node.import_account_backfill(std::slice::from_ref(&cleared))
        .unwrap();
    assert!(node.account_history(&peer, 10).is_empty());
    node.set_device_sync_category(crate::node::SyncCategory::History, true)
        .unwrap();
    let pruned = record(32, 50);
    node.import_account_backfill(std::slice::from_ref(&pruned))
        .unwrap();
    assert_eq!(node.prune_older_than(100).unwrap(), 1);
    drop(node);

    let node = open();
    node.import_account_backfill(&[cleared, pruned]).unwrap();
    assert!(node.account_history(&peer, 10).is_empty());
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
    use crate::node::conversation::account_conversation_id;
    use crate::node::dm_envelope::DmEnvelope;
    use crate::node::message::MessageBody;
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

    let contact = Account::generate();
    let conv = account_conversation_id(&linker_account.account_id(), &contact.account_id());
    let historical = DmEnvelope::new(
        contact.account_id(),
        linker_account.account_id(),
        [41; 32],
        MessageBody::new(b"before linking".to_vec(), None).encode(),
    )
    .encode();
    linker
        .received
        .lock()
        .unwrap()
        .record(
            conv,
            contact.account_id(),
            1,
            &historical,
            EventId::new([42; 32]),
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
    assert_eq!(joiner.received.lock().unwrap().entries(&conv).len(), 1);
    assert_eq!(linker.linked_devices().len(), 2);
    assert!(linker.linked_devices().iter().any(|device| {
        device.public.user_id() == joiner.user_id() && device.certificate.verify()
    }));
    let status = linker.device_sync_status();
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].device_id, joiner.user_id());
    assert_eq!(status[0].phase, SyncPhase::Offline);
    linker
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    linker
        .set_device_sync_category(SyncCategory::Profile, false)
        .unwrap();
    assert_eq!(linker.device_sync_status()[0].phase, SyncPhase::Disabled);
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

    // Exercise the large-file wire path with a small fixture: the only format
    // difference is the omitted hash list, so a huge test file is unnecessary.
    let (mut compact, compact_conv) = alice_node
        .stage_file(&file, crate::file::FileKind::File, |_| {})
        .unwrap();
    compact.v2.chunk_hashes.clear();
    let peer = alice_node.routing_peer(&bob_uid).unwrap();
    let host = dm_conversation_id(&alice_node.identity.public(), &peer.public);
    let card = alice_node
        .accept_staged_manifest(&[peer], host, None, &compact)
        .unwrap();
    let saved = dir.path().join("compact-copy.txt");
    alice_node
        .save_directory_archive(compact_conv, &saved)
        .unwrap();
    assert_eq!(std::fs::read(saved).unwrap(), b"account file payload");
    alice_node.flush_delivery(card).await;
    let received = tokio::time::timeout(std::time::Duration::from_secs(5), b_f_r.recv())
        .await
        .expect("bob received the compact manifest within 5s")
        .expect("file stream open");
    assert_eq!(received.file_conv, compact_conv);
    assert!(matches!(
        bob_node.files.lock().unwrap().manifest(&compact_conv),
        Some(crate::file::AnyManifest::V4(_))
    ));
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

    // A valid sealed reaction addressed to another account must not leak into
    // this account's aggregate, even when its logical target is reused.
    let wrong_route = crate::node::dm_envelope::ReactionEnvelope::new(
        alice_acct.account_id(),
        Account::generate().account_id(),
        *target.as_bytes(),
        "🔥".into(),
        false,
    );
    let peer = alice_node
        .roster
        .lock()
        .unwrap()
        .get(&bob_uid)
        .unwrap()
        .clone();
    let conv = dm_conversation_id(&alice_node.identity.public(), &peer.public);
    alice_node
        .append_event(
            conv,
            EventKind::React,
            crate::dm::seal(
                &alice_node.identity,
                &peer.public.x25519_pub,
                &wrong_route.encode(),
            )
            .unwrap(),
        )
        .unwrap();
    alice_node.deliver_direct(&peer, conv).await.unwrap();
    assert!(
        bob_node
            .log
            .lock()
            .unwrap()
            .events(&conv)
            .iter()
            .filter(|e| e.kind == EventKind::React)
            .count()
            >= 2
    );
    assert_eq!(
        bob_node.account_reactions(&alice_acct.account_id()),
        views,
        "reaction route must match the requested account conversation"
    );

    bob_node
        .roster
        .lock()
        .unwrap()
        .evict_stale(std::time::Duration::ZERO);
    assert_eq!(
        bob_node.account_reactions(&alice_acct.account_id()),
        views,
        "verified history keeps offline account reactions visible"
    );

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
    alice2_node.import_account_backfill(&records).unwrap();

    let hist = alice2_node.account_history(&bob_acct.account_id(), 10);
    assert_eq!(
        hist.len(),
        1,
        "the linked device sees the back-filled message"
    );
    assert!(hist[0].from_me, "it was sent by our (shared) account");
    assert_eq!(hist[0].text, b"old message");
}

// The CLI runs Node directly; its accept loop must also maintain the durable log.
#[tokio::test]
async fn accept_loop_compacts_received_profiles_without_desktop_runtime() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let alice_roster = seed_roster(
        &bob,
        "Bob",
        listener.local_addr().unwrap().port(),
        &alice.public().user_id(),
    );
    let bob_roster = seed_roster(&alice, "Alice", 4000, &bob.public().user_id());
    let dir = tempfile::tempdir().unwrap();
    let (a_dm, _a_rx) = mpsc::unbounded_channel();
    let (a_ch, _a_ch_rx) = mpsc::unbounded_channel();
    let (a_file, _a_file_rx) = mpsc::unbounded_channel();
    let (b_dm, mut b_rx) = mpsc::unbounded_channel();
    let (b_ch, _b_ch_rx) = mpsc::unbounded_channel();
    let (b_file, _b_file_rx) = mpsc::unbounded_channel();
    let alice_node = Node::open(
        alice,
        alice_roster,
        a_dm,
        a_ch,
        a_file,
        &dir.path().join("alice.log"),
        &dir.path().join("alice-sent.log"),
        "pw",
    )
    .unwrap();
    let bob_path = dir.path().join("bob.log");
    let bob_node = Node::open(
        bob,
        bob_roster,
        b_dm,
        b_ch,
        b_file,
        &bob_path,
        &dir.path().join("bob-sent.log"),
        "pw",
    )
    .unwrap();
    let conv = dm_conversation_id(&alice_node.identity.public(), &bob_node.identity.public());
    add_account_peer(
        &bob_node.roster,
        &alice_node.identity,
        &alice_node.account,
        "Alice",
        4000,
        &bob_node.identity.public().user_id(),
    );
    // An ordinary message must survive reclaiming eight superseded profiles;
    // the two newest profile leaves remain for signed-card/avatar compatibility.
    let message = alice_node
        .dm_ratchet
        .lock()
        .unwrap()
        .encrypt(
            &alice_node.identity,
            &bob_node.identity.public(),
            &MessageBody::new(b"keep me".to_vec(), None).encode(),
        )
        .unwrap();
    alice_node
        .append_event(conv, EventKind::Message, message)
        .unwrap();
    for i in 0..10 {
        let profile = crate::node::profile::ProfilePayload::new(
            &alice_node.account,
            Some(vec![i]),
            u64::from(i) + 1,
        );
        alice_node
            .append_event(
                conv,
                EventKind::Profile,
                crate::dm::seal(
                    &alice_node.identity,
                    &bob_node.identity.public().x25519_pub,
                    &profile.encode(),
                )
                .unwrap(),
            )
            .unwrap();
    }
    let accept = tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));
    let peer = alice_node.roster.lock().unwrap().peers()[0].clone();
    alice_node.deliver_direct(&peer, conv).await.unwrap();
    let received = tokio::time::timeout(Duration::from_secs(5), b_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received.text, b"keep me");
    let compacted = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if bob_node.log.lock().unwrap().conversation_len(&conv) == 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    accept.abort();
    let _ = accept.await;
    assert!(
        compacted.is_ok(),
        "the CLI accept loop must reclaim superseded profiles"
    );
    assert_eq!(
        bob_node.peer_avatar(&alice_node.account.account_id()),
        Some(vec![9])
    );
    let reopened = PersistentEventLog::open(&bob_path, "pw").unwrap();
    assert_eq!(reopened.conversation_len(&conv), 3);
    assert_eq!(
        reopened
            .events(&conv)
            .iter()
            .filter(|e| e.kind == EventKind::Message)
            .count(),
        1
    );
    // Compaction must leave the live append handle attached to the rewritten file.
    bob_node
        .append_event(conv, EventKind::Message, b"after compaction".to_vec())
        .unwrap();
    let reopened = PersistentEventLog::open(&bob_path, "pw").unwrap();
    assert_eq!(reopened.conversation_len(&conv), 4);
}

#[tokio::test]
async fn signed_avatar_reaches_a_peer_through_a_post_office_without_direct_tcp() {
    use crate::identity::account::Account;
    use crate::postoffice::opaque::RelayMailbox;

    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let alice_account = Account::generate();
    let alice_account_id = alice_account.account_id();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);
    let alice_proof = Announce::new_with_account(&alice, &alice_account, "Alice", dead_port);
    let bob_proof = Announce::new_with_account(&bob, &bob_account, "Bob", dead_port);
    let (sender, _, _) = crate::node::test_support::node_with_files(
        &dir.path().join("alice"),
        alice,
        alice_account,
        &bob_proof,
    );
    let (receiver, mut incoming, _) = crate::node::test_support::node_with_files(
        &dir.path().join("bob"),
        bob,
        bob_account,
        &alice_proof,
    );
    sender.set_profile_display_name("Alice").unwrap();
    let avatar = format!("data:image/jpeg;base64,{}", "A".repeat(32 * 1024)).into_bytes();
    // No relay is known yet: prepare the signed events without uploading them.
    sender.set_avatar(Some(avatar.clone())).await.unwrap();
    let conv = dm_conversation_id(&sender.identity.public(), &receiver.identity.public());
    let events: Vec<_> = sender
        .log
        .lock()
        .unwrap()
        .events(&conv)
        .into_iter()
        .cloned()
        .collect();
    assert_eq!(events.len(), 2);
    // Opaque IDs conceal causal order. Select a relay salt which puts the child
    // first, and use large avatars so the two wrappers require separate pages.
    let relay = (0..32)
        .find_map(|_| {
            let candidate = DeviceIdentity::generate();
            let mailbox = RelayMailbox::new(
                &sender.identity,
                &receiver.identity.public(),
                &candidate.public(),
            )
            .unwrap();
            let parent = mailbox.wrap(&events[0]).unwrap();
            let child = mailbox.wrap(&events[1]).unwrap();
            assert!(
                parent.ciphertext.len() + child.ciphertext.len() > crate::transport::MAX_PLAINTEXT
            );
            (child.id < parent.id).then_some(candidate)
        })
        .expect("find a child-first opaque mailbox ordering");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (ed, x) = relay.secret_bytes();
    let relay_announcement =
        Announce::new_post_office(&relay, "Relay", listener.local_addr().unwrap().port());
    let relay_store = Arc::new(Mutex::new(
        PostOffice::open(&dir.path().join("relay.log"), "pw", relay).unwrap(),
    ));
    let relay_task = tokio::spawn(run_relay_accept_loop(
        DeviceIdentity::from_secret_bytes(ed, x),
        listener,
        relay_store,
    ));
    for node in [&sender, &receiver] {
        node.roster.lock().unwrap().update(
            &relay_announcement,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &node.user_id(),
        );
    }
    receiver.replace_verified_dm_pull_peers(vec![alice_proof.public().user_id()]);
    sender.replace_verified_dm_pull_peers(vec![bob_proof.public().user_id()]);
    // A queued chat message shares this mailbox with the large signed profile.
    // Child-first profile pages must not starve content or its exact receipt.
    let message = sender
        .enqueue_to_account(
            &receiver.account_id(),
            b"message behind a large avatar",
            None,
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !sender.replicate_to_post_office(conv).await.unwrap_or(false) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the relay must accept the signed profile events");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            receiver.drain_from_post_office().await;
            if incoming
                .try_recv()
                .is_ok_and(|dm| dm.text == b"message behind a large avatar")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("a large avatar must not starve a message in the same relay mailbox");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            receiver.drain_from_post_office().await;
            if receiver.peer_avatar(&alice_account_id).as_ref() == Some(&avatar)
                && receiver.log.lock().unwrap().has(&events[1].id)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("child-first relay pages must not prevent receipt of the signed avatar");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            receiver
                .replicate_to_post_office(crate::node::delivery_receipt::delivery_conversation_id(
                    &sender.identity.public(),
                    &receiver.identity.public(),
                ))
                .await
                .unwrap();
            sender.drain_from_post_office().await;
            if sender.delivery_status(message) == Some(crate::node::DeliveryStatus::Delivered) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the recipient receipt must return through the relay without direct TCP");
    assert_eq!(
        receiver.account_profile(&alice_account_id).unwrap().avatar,
        Some(avatar)
    );
    relay_task.abort();
}

#[tokio::test]
async fn signed_avatar_retries_after_relay_publication_is_reenabled() {
    use crate::identity::account::Account;
    use crate::node::runtime::NodeRuntime;

    let dir = tempfile::tempdir().unwrap();
    let discovery = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let discovery_port = discovery.local_addr().unwrap().port();
    drop(discovery);
    let runtime = NodeRuntime::start(
        &dir.path().join("sender"),
        "alice",
        "Alice",
        "pw",
        discovery_port,
        |_| {},
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await
    .unwrap();
    let sender = runtime.handle();
    sender.set_post_office_permissions(false, true);
    let avatar = b"data:image/jpeg;base64,QUJD".to_vec();
    sender.set_avatar(Some(avatar.clone())).await.unwrap();

    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);
    let sender_proof =
        Announce::new_with_account(&sender.identity, &sender.account, "Alice", dead_port);
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let bob_proof = Announce::new_with_account(&bob, &bob_account, "Bob", dead_port);
    let (receiver, _, _) = crate::node::test_support::node_with_files(
        &dir.path().join("receiver"),
        bob,
        bob_account,
        &sender_proof,
    );
    receiver.replace_verified_dm_pull_peers(vec![sender.user_id()]);
    let relay = DeviceIdentity::generate();
    let relay_account = Account::generate();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_proof = Announce::new_post_office_with_account(
        &relay,
        &relay_account,
        "Relay",
        listener.local_addr().unwrap().port(),
    );
    let (ed, x) = relay.secret_bytes();
    let relay_store = Arc::new(Mutex::new(
        PostOffice::open(&dir.path().join("relay.log"), "pw", relay).unwrap(),
    ));
    let relay_task = tokio::spawn(run_relay_accept_loop(
        DeviceIdentity::from_secret_bytes(ed, x),
        listener,
        relay_store,
    ));
    // An older peer authenticates but never answers a sync request. It must not
    // hold up profile publication to the healthy recipient later in the roster.
    let stalled_identity = DeviceIdentity::generate();
    let stalled_account = Account::generate();
    let stalled_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stalled_proof = Announce::new_with_account(
        &stalled_identity,
        &stalled_account,
        "Stalled",
        stalled_listener.local_addr().unwrap().port(),
    );
    let stalled_task = tokio::spawn(async move {
        let (stream, _) = stalled_listener.accept().await.unwrap();
        let _channel = crate::transport::SecureChannel::accept(stream, &stalled_identity)
            .await
            .unwrap();
        std::future::pending::<()>().await;
    });
    for node in [&sender, &receiver] {
        node.roster.lock().unwrap().update(
            &relay_proof,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &node.user_id(),
        );
    }
    sender.roster.lock().unwrap().update(
        &stalled_proof,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &sender.user_id(),
    );
    sender.roster.lock().unwrap().update(
        &bob_proof,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &sender.user_id(),
    );
    let conv = dm_conversation_id(&sender.identity.public(), &receiver.identity.public());
    tokio::time::timeout(Duration::from_secs(6), async {
        while sender.log.lock().unwrap().events(&conv).is_empty() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the runtime must attempt its first profile publication");
    receiver.drain_from_post_office().await;
    assert_eq!(receiver.peer_avatar(&sender.account_id()), None);
    let prepared_ids: Vec<_> = sender
        .log
        .lock()
        .unwrap()
        .events(&conv)
        .iter()
        .map(|event| event.id)
        .collect();
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert_eq!(
        sender
            .log
            .lock()
            .unwrap()
            .events(&conv)
            .iter()
            .map(|event| event.id)
            .collect::<Vec<_>>(),
        prepared_ids,
        "failed retries must reuse the accepted profile events without log churn"
    );
    sender.set_post_office_permissions(true, true);

    let recovered = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            receiver.drain_from_post_office().await;
            if receiver.peer_avatar(&sender.account_id()).as_ref() == Some(&avatar) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    if recovered.is_err() {
        runtime.stop().await;
        relay_task.abort();
        stalled_task.abort();
        panic!("an unchanged peer must receive the pending avatar after relay recovery");
    }

    // A failed clear is also a pending revision for an already-present peer.
    sender.set_post_office_permissions(false, true);
    sender.set_avatar(None).await.unwrap();
    receiver.drain_from_post_office().await;
    assert_eq!(receiver.peer_avatar(&sender.account_id()), Some(avatar));
    sender.set_post_office_permissions(true, true);
    let cleared = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            receiver.drain_from_post_office().await;
            if receiver.peer_profile_states().get(&sender.account_id()) == Some(&None) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    runtime.stop().await;
    relay_task.abort();
    stalled_task.abort();
    cleared.expect("a pending clear must propagate without a peer reconnect or another edit");
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
    assert_eq!(got, Some(avatar.clone()), "bob received alice's avatar");

    let profile_conv = crate::node::conversation::dm_conversation_id(
        &alice_node.identity.public(),
        &bob_node.identity.public(),
    );
    let before_duplicate = bob_node.log.lock().unwrap().events(&profile_conv).len();
    alice_node.set_avatar(Some(avatar.clone())).await.unwrap();
    let after_duplicate = bob_node.log.lock().unwrap().events(&profile_conv).len();
    assert_eq!(
        after_duplicate, before_duplicate,
        "setting the unchanged avatar must not append another profile event"
    );

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
        .account_profiles
        .restore_avatar(
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
async fn own_signed_profile_and_clear_survive_restart_for_new_peers() {
    let identity = DeviceIdentity::generate();
    let account = crate::identity::account::Account::generate();
    let dir = tempfile::tempdir().unwrap();
    let open = || {
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, _) = mpsc::unbounded_channel();
        let (file, _) = mpsc::unbounded_channel();
        Node::open_with_account(
            DeviceIdentity::from_secret_bytes(identity.secret_bytes().0, identity.secret_bytes().1),
            crate::identity::account::Account::from_secret_bytes(account.secret_bytes()),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            &dir.path().join("me.log"),
            &dir.path().join("me-sent.log"),
            "pw",
        )
        .unwrap()
    };
    let node = open();
    node.set_avatar(Some(b"data:image/png;base64,QQ==".to_vec()))
        .await
        .unwrap();
    drop(node);
    let node = open();
    let profile = node.account_profiles.current().unwrap();
    assert_eq!(profile.avatar, Some(b"data:image/png;base64,QQ==".to_vec()));
    assert!(profile.verify());
    node.set_avatar(None).await.unwrap();
    drop(node);
    let node = open();
    let profile = node.account_profiles.current().unwrap();
    assert_eq!(profile.avatar, None);
    assert!(profile.verify());
    assert_eq!(
        node.peer_profile_states().get(&account.account_id()),
        Some(&None)
    );
}

// An oversized avatar is rejected at the publish boundary (never sealed/appended).
#[tokio::test]
async fn disabled_profile_sync_defers_sibling_avatar_until_enabled() {
    let account = crate::identity::account::Account::generate();
    let source = DeviceIdentity::generate();
    let target = DeviceIdentity::generate();
    let dir = tempfile::tempdir().unwrap();
    let roster = Arc::new(Mutex::new(Roster::default()));
    add_account_peer(
        &roster,
        &source,
        &account,
        "Sibling",
        4000,
        &target.public().user_id(),
    );
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    let node = Node::open_with_account(
        target,
        crate::identity::account::Account::from_secret_bytes(account.secret_bytes()),
        roster,
        dm,
        channel,
        file,
        &dir.path().join("target.log"),
        &dir.path().join("target-sent.log"),
        "pw",
    )
    .unwrap();
    node.set_device_sync_category(crate::node::SyncCategory::Profile, false)
        .unwrap();
    let avatar = b"data:image/png;base64,QQ==".to_vec();
    let profile = crate::node::profile::ProfilePayload::new(&account, Some(avatar.clone()), 42);
    let conversation = dm_conversation_id(&source.public(), &node.identity.public());
    let ciphertext = crate::dm::seal(
        &source,
        &node.identity.public().x25519_pub,
        &profile.encode(),
    )
    .unwrap();
    let event = Event::new(
        &source,
        conversation,
        1,
        vec![],
        1,
        42,
        EventKind::Profile,
        ciphertext,
    );
    node.log.lock().unwrap().append(event).unwrap();
    node.process_profile_events(conversation);
    assert_eq!(node.peer_avatar(&account.account_id()), None);
    node.set_device_sync_category(crate::node::SyncCategory::Profile, true)
        .unwrap();
    assert_eq!(node.peer_avatar(&account.account_id()), Some(avatar));
    assert_eq!(node.account_profiles.current().unwrap().updated_at, 42);
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

#[tokio::test]
async fn sibling_history_policy_retains_ciphertext_and_replays_without_network() {
    use crate::identity::account::Account;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = personal_message_node(
        &dir.path().join("source"),
        DeviceIdentity::generate(),
        Account::from_secret_bytes(account.secret_bytes()),
    );
    let receiver = personal_message_node(
        &dir.path().join("receiver"),
        DeviceIdentity::generate(),
        account,
    );
    for (owner, other) in [(&source, &receiver), (&receiver, &source)] {
        owner
            .account_devices
            .lock()
            .unwrap()
            .insert(crate::node::account_devices::AccountDevice {
                public: other.identity.public(),
                certificate: owner.account.certify(&other.identity.public().ed25519_pub),
            })
            .unwrap();
        add_account_peer(
            &owner.roster,
            &other.identity,
            &other.account,
            "sibling",
            9,
            &owner.user_id(),
        );
    }
    let target = source.account_id();
    let id = source
        .enqueue_to_account(&target, b"retained while paused", None)
        .await
        .unwrap();
    let destination = source
        .delivery
        .lock()
        .unwrap()
        .message(id)
        .unwrap()
        .destinations[0]
        .clone();
    receiver
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    receiver
        .log
        .lock()
        .unwrap()
        .append(destination.event.clone())
        .unwrap();
    receiver.emit_new_messages(destination.event.conversation_id);
    assert!(
        receiver.account_history(&target, 10).is_empty(),
        "paused sibling text must remain ciphertext"
    );
    assert!(
        !receiver
            .emitted
            .lock()
            .unwrap()
            .contains(&destination.event.id)
    );
    receiver
        .set_device_sync_category(SyncCategory::History, true)
        .unwrap();
    assert_eq!(
        receiver.account_history(&target, 10).len(),
        1,
        "reenable must replay retained text even without a reachable source"
    );
    source
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    assert!(
        !source.delivery_destination_allowed(&destination),
        "queued sibling text must pause"
    );
    assert!(
        source.account_fanout_targets(&target).is_empty(),
        "new sibling text must pause"
    );
    use crate::eventlog::sync::SyncStore;
    let view = source.serve_sync_store(&receiver.identity.public());
    assert!(
        view.lock()
            .unwrap()
            .events_excluding(
                &destination.event.conversation_id,
                &std::collections::HashSet::new()
            )
            .is_empty(),
        "ordinary sync must not bypass source history policy"
    );
    let outsider = DeviceIdentity::generate();
    let outside_account = Account::generate();
    add_account_peer(
        &source.roster,
        &outsider,
        &outside_account,
        "outside",
        9,
        &source.user_id(),
    );
    let external = source
        .enqueue_to_account(
            &outside_account.account_id(),
            b"ordinary DM remains live",
            None,
        )
        .await
        .unwrap();
    let outside_destination = source
        .delivery
        .lock()
        .unwrap()
        .message(external)
        .unwrap()
        .destinations[0]
        .clone();
    assert!(source.delivery_destination_allowed(&outside_destination));
}

#[tokio::test]
async fn sibling_file_retry_uses_attachment_category_independently_of_history() {
    use crate::identity::account::Account;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = personal_message_node(
        &dir.path().join("source"),
        DeviceIdentity::generate(),
        Account::from_secret_bytes(account.secret_bytes()),
    );
    let sibling = personal_message_node(
        &dir.path().join("sibling"),
        DeviceIdentity::generate(),
        account,
    );
    source
        .account_devices
        .lock()
        .unwrap()
        .insert(crate::node::account_devices::AccountDevice {
            public: sibling.identity.public(),
            certificate: source
                .account
                .certify(&sibling.identity.public().ed25519_pub),
        })
        .unwrap();
    add_account_peer(
        &source.roster,
        &sibling.identity,
        &sibling.account,
        "sibling",
        9,
        &source.user_id(),
    );
    let event = crate::eventlog::Event::new(
        &source.identity,
        crate::node::conversation::dm_conversation_id(
            &source.identity.public(),
            &sibling.identity.public(),
        ),
        1,
        vec![],
        1,
        1,
        EventKind::FileManifest,
        vec![1],
    );
    let destination = crate::node::delivery_store::DeliveryDestination {
        device: sibling.identity.public(),
        account: Some(source.account_id()),
        event,
        receipt_eligible: false,
    };
    assert!(
        !source.delivery_destination_allowed(&destination),
        "default bytes-off must pause old sibling file retries"
    );
    source
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    source
        .set_device_sync_category(SyncCategory::Attachments, true)
        .unwrap();
    assert!(
        source.delivery_destination_allowed(&destination),
        "files-only mode remains allowed"
    );
}

#[tokio::test]
async fn group_reception_history_respects_signed_ancestry_despite_clock_skew() {
    let dir = tempfile::tempdir().unwrap();
    let author = DeviceIdentity::generate();
    let (dm, _) = mpsc::unbounded_channel();
    let (channel_tx, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    let node = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel_tx,
        file,
        &dir.path().join("node.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let conv = node
        .create_channel("skew", vec![author.public()])
        .await
        .unwrap();
    node.pull_one_channel_from_member(0).await;
    let recovery = node.channel_recovery(conv).unwrap();
    assert_eq!(recovery.phase, "waiting_for_peer");
    assert_eq!(recovery.checked_at, None);

    let (parents, _) = node.log.lock().unwrap().prepare(&conv).unwrap();
    let question = Event::new(
        &author,
        conv,
        1,
        parents,
        900,
        9000,
        EventKind::Message,
        vec![],
    );
    // Valid signatures/ancestry but deliberately non-monotone Lamport and wall clocks.
    let reply = Event::new(
        &author,
        conv,
        2,
        vec![question.id],
        1,
        1,
        EventKind::Message,
        vec![],
    );
    for (event, text, parent) in [
        (&question, b"question".as_slice(), None),
        (&reply, b"reply".as_slice(), Some(question.id)),
    ] {
        node.log.lock().unwrap().append(event.clone()).unwrap();
        node.received
            .lock()
            .unwrap()
            .record(
                conv,
                author.public().user_id(),
                event.wall_clock,
                &MessageBody::new(text.to_vec(), parent).encode(),
                event.id,
            )
            .unwrap();
    }
    let history = node.channel_history(conv, 10);
    assert_eq!(
        history.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        vec![question.id, reply.id]
    );
    assert_eq!(
        history
            .iter()
            .map(|entry| entry.wall_clock)
            .collect::<Vec<_>>(),
        vec![9000, 1]
    );
    assert_eq!(node.channel_history(conv, 1)[0].id, reply.id);

    let archive = crate::channel::new_channel_id();
    let genesis = Event::new(
        &author,
        archive,
        1,
        vec![],
        900,
        9000,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "archived skew".into(),
            members: vec![author.public()],
            epoch: 0,
            owner: author.public().user_id(),
        }
        .encode(),
    );
    let dissolve = Event::new(
        &author,
        archive,
        2,
        vec![genesis.id],
        1,
        1,
        EventKind::ChannelDissolve,
        vec![],
    );
    let mut log = node.log.lock().unwrap();
    log.append(genesis).unwrap();
    log.append(dissolve).unwrap();
    let summaries = archived_channel_summaries(&log, &crate::node::channel::ChannelBook::default());
    assert!(
        summaries
            .iter()
            .any(|summary| summary.id == archive && summary.archived && summary.dissolved)
    );
}

#[tokio::test]
async fn group_reception_multiple_chunks_complete_once_and_reopen_preserves_erases() {
    group_reception_projection_failure_case(false).await;
}

#[tokio::test]
async fn group_reception_failed_rollback_retries_without_new_traffic() {
    group_reception_projection_failure_case(true).await;
}

async fn group_reception_projection_failure_case(failed_rollback: bool) {
    let dir = tempfile::tempdir().unwrap();
    let owner = DeviceIdentity::generate();
    let receiver = DeviceIdentity::generate();
    let secret = receiver.secret_bytes();
    let public = receiver.public();
    let open = |identity| {
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, rx) = mpsc::unbounded_channel();
        let (file, _) = mpsc::unbounded_channel();
        (
            Node::open(
                identity,
                Arc::new(Mutex::new(Roster::default())),
                dm,
                channel,
                file,
                &dir.path().join("node.log"),
                &dir.path().join("sent.log"),
                "pw",
            )
            .unwrap(),
            rx,
        )
    };
    let (node, mut rx) = open(receiver);
    let conv = crate::channel::new_channel_id();
    let meta = ChannelMeta {
        name: "backlog".into(),
        members: vec![owner.public(), public],
        epoch: 0,
        owner: owner.public().user_id(),
    };
    let append = |seq, kind, bytes| {
        let mut log = node.log.lock().unwrap();
        let (parents, clock) = log.prepare(&conv).unwrap();
        let event = Event::new(&owner, conv, seq, parents, clock, seq, kind, bytes);
        let id = event.id;
        log.append(event).unwrap();
        id
    };
    append(1, EventKind::MembershipChange, meta.encode());
    let mut sender = crate::channel::ChannelState::from_meta(conv, meta.clone());
    sender.set_identity(owner.public().user_id());
    let keys = crate::node::channel::seal_keys_for(
        &owner,
        &meta.members,
        &sender.my_sender_distribution(),
        0,
    )
    .unwrap();
    append(2, EventKind::KeyRotation, keys.encode());
    let mut ids = Vec::new();
    for index in 0..65 {
        let body = MessageBody::new(format!("backlog {index}").into_bytes(), None).encode();
        ids.push(append(
            index + 3,
            EventKind::Message,
            sender.seal_sender_message(&body).unwrap(),
        ));
    }
    append(68, EventKind::ChannelDissolve, vec![]);
    if failed_rollback {
        node.received
            .lock()
            .unwrap()
            .fail_next_append_rollback_for_test(7);
    } else {
        node.received.lock().unwrap().fail_once_after_for_test(1);
    }
    node.process_channel_reception(conv, "catch_up");
    let prefix = if failed_rollback {
        assert!(node.channel_history(conv, 100).is_empty());
        None
    } else {
        assert_eq!(node.channel_history(conv, 100).len(), 1);
        let prefix = rx.try_recv().unwrap();
        assert_eq!(prefix.event_id, ids[0]);
        assert!(!prefix.reception_only && !prefix.batch_complete);
        Some(prefix)
    };
    assert!(
        rx.try_recv().is_err(),
        "a failed suffix must not be published or complete the batch"
    );
    if failed_rollback {
        // No new signed traffic or manual plaintext retry: the serving host's
        // existing recovery timer must repair storage and finish publication.
        let recovery = tokio::spawn(
            node.clone()
                .run_channel_recovery_loop(Arc::new(tokio::sync::Notify::new())),
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while node.channel_history(conv, 100).len() != 65 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("background recovery must resume after storage recovers");
        recovery.abort();
        let _ = recovery.await;
    } else {
        tokio::time::sleep(Duration::from_millis(1100)).await;
        node.retry_channel_plaintext();
    }
    let events = prefix
        .into_iter()
        .chain(std::iter::from_fn(|| rx.try_recv().ok()));
    let (terminals, received): (Vec<_>, Vec<_>) = events.partition(|event| event.reception_only);
    assert_eq!(terminals.len(), 1);
    assert!(terminals[0].batch_complete);
    assert_eq!(terminals[0].batch_size, 65);
    assert!(terminals[0].text.is_empty() && terminals[0].wrapped.is_empty());
    assert!(!terminals[0].own_device);
    assert_eq!(terminals[0].batch_id, received[0].batch_id);
    assert_eq!(received.len(), 65);
    assert_eq!(
        received
            .iter()
            .map(|message| message.event_id)
            .collect::<Vec<_>>(),
        ids,
        "partial projection failure must preserve publication order"
    );
    assert_eq!(
        received
            .iter()
            .map(|message| message.event_id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        65
    );
    assert!(
        received
            .iter()
            .all(|message| message.batch_id == received[0].batch_id)
    );
    assert_eq!(
        received
            .iter()
            .filter(|message| message.batch_complete)
            .count(),
        0
    );
    assert!(!received.last().unwrap().batch_complete);
    assert_eq!(received.last().unwrap().batch_size, 65);
    assert_eq!(node.channel_history(conv, 100).len(), 65);
    assert!(node.list_channels()[0].dissolved);

    // Simulate a sidecar projection missing after a crash, plus a separate
    // intentional erase. Startup repairs only the missing projection silently.
    node.received
        .lock()
        .unwrap()
        .remove_where(|entry| entry.event_id == ids[0])
        .unwrap();
    node.delete_message(conv, ids[1], false).unwrap();
    drop(node);
    let (reopened, mut reopened_rx) = open(DeviceIdentity::from_secret_bytes(secret.0, secret.1));
    let history = reopened.channel_history(conv, 100);
    assert_eq!(history.len(), 64);
    assert!(history.iter().any(|entry| entry.id == ids[0]));
    assert!(!history.iter().any(|entry| entry.id == ids[1]));
    assert!(
        reopened_rx.try_recv().is_err(),
        "restart repair must not replay notifications"
    );
    // Clear must also cover a retained signed row whose projection is absent.
    reopened
        .received
        .lock()
        .unwrap()
        .remove_where(|entry| entry.event_id == ids[2])
        .unwrap();
    reopened.clear_conversation(conv).unwrap();
    drop(reopened);
    let (cleared, mut cleared_rx) = open(DeviceIdentity::from_secret_bytes(secret.0, secret.1));
    assert!(cleared.channel_history(conv, 100).is_empty());
    assert!(cleared_rx.try_recv().is_err());
}

#[tokio::test]
async fn group_reception_terminal_is_independent_of_erased_and_expired_rows() {
    let dir = tempfile::tempdir().unwrap();
    let owner = DeviceIdentity::generate();
    let receiver = DeviceIdentity::generate();
    let meta = ChannelMeta {
        name: "terminal".into(),
        members: vec![owner.public(), receiver.public()],
        epoch: 0,
        owner: owner.public().user_id(),
    };
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, mut rx) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    let node = Node::open(
        receiver,
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        file,
        &dir.path().join("node.log"),
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let conv = crate::channel::new_channel_id();
    let append = |seq, kind, bytes| {
        let mut log = node.log.lock().unwrap();
        let (parents, clock) = log.prepare(&conv).unwrap();
        let event = Event::new(
            &owner,
            conv,
            seq,
            parents,
            clock,
            crate::node::node::now_millis(),
            kind,
            bytes,
        );
        log.append(event).unwrap();
    };
    append(1, EventKind::MembershipChange, meta.encode());
    let mut sender = crate::channel::ChannelState::from_meta(conv, meta.clone());
    sender.set_identity(owner.public().user_id());
    let keys = crate::node::channel::seal_keys_for(
        &owner,
        &meta.members,
        &sender.my_sender_distribution(),
        0,
    )
    .unwrap();
    append(2, EventKind::KeyRotation, keys.encode());
    for index in 0..3 {
        append(
            index + 3,
            EventKind::Message,
            sender
                .seal_sender_message(format!("erase {index}").as_bytes())
                .unwrap(),
        );
    }
    node.received.lock().unwrap().fail_once_after_for_test(1);
    node.process_channel_reception(conv, "catch_up");
    let published = rx.try_recv().unwrap();
    assert_eq!(published.text, b"erase 0");
    assert!(!published.batch_complete);
    node.clear_conversation(conv).unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    node.retry_channel_plaintext();
    let terminal = rx.try_recv().unwrap();
    assert!(terminal.reception_only && terminal.batch_complete);
    assert_eq!(terminal.batch_id, published.batch_id);
    assert!(terminal.text.is_empty() && terminal.wrapped.is_empty());
    assert_eq!(terminal.from, owner.public().user_id());
    assert!(
        !terminal.own_device,
        "a terminal must retain real ownership metadata"
    );
    assert!(rx.try_recv().is_err());
    assert!(node.channel_history(conv, 10).is_empty());

    let expiring = crate::expiry::wrap(
        &MessageBody::new(b"expiring prefix".to_vec(), None).encode(),
        Some(crate::node::node::now_millis() + 500),
    );
    append(
        6,
        EventKind::Message,
        sender.seal_sender_message(&expiring).unwrap(),
    );
    append(
        7,
        EventKind::Message,
        sender.seal_sender_message(b"retained suffix").unwrap(),
    );
    node.received.lock().unwrap().fail_once_after_for_test(1);
    node.process_channel_reception(conv, "catch_up");
    let published = rx.try_recv().unwrap();
    assert_eq!(published.text, b"expiring prefix");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    node.retry_channel_plaintext();
    let remainder: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let visible: Vec<_> = remainder
        .iter()
        .filter(|event| !event.reception_only)
        .collect();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].text, b"retained suffix");
    assert!(
        remainder
            .iter()
            .any(|event| event.reception_only && event.batch_complete)
    );
    assert!(
        remainder
            .iter()
            .all(|event| event.text != b"expiring prefix")
    );
}

#[tokio::test]
async fn own_account_reactions_survive_restart_and_removal() {
    use crate::identity::account::Account;
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let secret = identity.secret_bytes();
    let account = Account::generate();
    let peer = Account::generate().account_id();
    let open = || {
        let (dm, _) = mpsc::unbounded_channel();
        let (ch, _) = mpsc::unbounded_channel();
        let (file, _) = mpsc::unbounded_channel();
        Node::open_with_account(
            DeviceIdentity::from_secret_bytes(secret.0, secret.1),
            Account::from_secret_bytes(account.secret_bytes()),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            ch,
            file,
            &dir.path().join("events.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap()
    };
    let target = EventId::new([42; 32]);
    let node = open();
    assert!(
        node.react_to_account_if(&peer, target, "blocked", false, |_| Err(
            NodeError::Authorization("session changed".into())
        ))
        .await
        .is_err()
    );
    assert!(
        node.account_reactions(&peer).is_empty(),
        "denied action has no projection"
    );
    assert!(
        node.log.lock().unwrap().conversations().is_empty(),
        "denied action has no signed event"
    );
    node.react_to_account(&peer, target, "👍", false)
        .await
        .unwrap();
    let expected = node.account_reactions(&peer);
    assert_eq!(expected.len(), 1);
    drop(node);
    let node = open();
    assert_eq!(
        node.account_reactions(&peer),
        expected,
        "own add survives restart"
    );
    node.my_dm_reactions.lock().unwrap().fail_appends_for_test();
    assert!(
        node.react_to_account(&peer, target, "🔥", false)
            .await
            .is_err(),
        "projection persistence failures reach the caller"
    );
    assert_eq!(
        node.account_reactions(&peer),
        expected,
        "failed add is invisible"
    );
    drop(node);
    let node = open();

    node.react_to_account(&peer, target, "👍", true)
        .await
        .unwrap();
    drop(node);
    assert!(
        open().account_reactions(&peer).is_empty(),
        "own remove survives restart"
    );
    let node = open();
    let other = EventId::new([43; 32]);
    node.react_to_account(&peer, other, "🎉", false)
        .await
        .unwrap();
    node.delete_account_message(&peer, other).unwrap();
    assert_eq!(
        node.my_dm_reactions.lock().unwrap().len(),
        1,
        "erasing a message physically removes its local reaction projection"
    );
    drop(node);
    assert!(
        open().account_reactions(&peer).is_empty(),
        "local erase removes durable reactions"
    );
    let node = open();
    node.react_to_account(&peer, EventId::new([44; 32]), "✅", false)
        .await
        .unwrap();
    node.clear_account_conversation(&peer).unwrap();
    drop(node);
    assert_eq!(
        open().my_dm_reactions.lock().unwrap().len(),
        0,
        "clearing conversation removes durable actions and tombstones"
    );
}

#[tokio::test]
async fn own_dm_reactions_survive_restart_and_removal() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let secret = identity.secret_bytes();
    let peer = DeviceIdentity::generate();
    let conv = dm_conversation_id(&identity.public(), &peer.public());
    let open = || {
        let (dm, _) = mpsc::unbounded_channel();
        let (ch, _) = mpsc::unbounded_channel();
        let (file, _) = mpsc::unbounded_channel();
        Node::open(
            DeviceIdentity::from_secret_bytes(secret.0, secret.1),
            seed_roster(&peer, "Peer", 1, &identity.user_id()),
            dm,
            ch,
            file,
            &dir.path().join("events.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap()
    };
    let target = EventId::new([42; 32]);
    let node = open();
    assert!(
        node.react_dm_if(&peer.user_id(), target, "blocked", false, |_| Err(
            NodeError::Authorization("session changed".into())
        ))
        .await
        .is_err()
    );
    assert!(
        node.reactions(conv).is_empty(),
        "denied DM reaction has no projection"
    );
    assert!(
        node.log.lock().unwrap().conversations().is_empty(),
        "denied DM reaction has no event"
    );
    node.react_dm(&peer.user_id(), target, "👍", false)
        .await
        .unwrap();
    let expected = node.reactions(conv);
    assert_eq!(expected.len(), 1);
    drop(node);
    let node = open();
    assert_eq!(
        node.reactions(conv),
        expected,
        "own DM add survives restart"
    );
    node.react_dm(&peer.user_id(), target, "👍", true)
        .await
        .unwrap();
    drop(node);
    assert!(
        open().reactions(conv).is_empty(),
        "own DM remove survives restart"
    );
}

// Archived recovery histories must use the same signed metadata projection as live channels.
#[test]
fn archived_metadata_honors_leave_epoch_rename_and_dissolution() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = PersistentEventLog::open(&dir.path().join("archive.log"), "pw").unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let channel = crate::channel::new_channel_id();
    let append = |log: &mut PersistentEventLog, author: &DeviceIdentity, kind, payload| {
        let (parents, clock) = log.prepare(&channel).unwrap();
        let seq = log
            .next_sequence(&channel, &Author::from_ed25519(author.public().ed25519_pub))
            .unwrap();
        log.append(Event::new(
            author, channel, seq, parents, clock, 1, kind, payload,
        ))
        .unwrap();
    };
    let meta = ChannelMeta {
        name: "Original".into(),
        members: vec![alice.public(), bob.public()],
        epoch: 0,
        owner: alice.public().user_id(),
    };
    append(&mut log, &alice, EventKind::MembershipChange, meta.encode());
    append(&mut log, &bob, EventKind::ChannelLeave, vec![]);
    // An old epoch snapshot cannot restore Bob or rename the channel.
    append(&mut log, &alice, EventKind::MembershipChange, meta.encode());
    append(
        &mut log,
        &alice,
        EventKind::ChannelRename,
        b"Renamed".to_vec(),
    );
    append(&mut log, &alice, EventKind::ChannelDissolve, vec![]);
    append(
        &mut log,
        &alice,
        EventKind::ChannelRename,
        b"Too late".to_vec(),
    );
    let mut book = ChannelBook::new();
    book.process(&alice, channel, &log.events(&channel));
    let live = book.state(&channel).unwrap();
    let archived = archived_channel_summaries(&log, &ChannelBook::new());
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].member_count, live.members().len());
    assert_eq!(archived[0].member_count, 1);
    assert_eq!(archived[0].owner, live.owner());
    assert_eq!(archived[0].name, live.name());
    assert!(archived[0].dissolved);
}

#[test]
fn archived_metadata_rejects_transfer_to_unaccepted_invitee() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = PersistentEventLog::open(&dir.path().join("archive.log"), "pw").unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let channel = crate::channel::new_channel_id();
    let meta = ChannelMeta {
        name: "Invited".into(),
        members: vec![alice.public(), bob.public()],
        epoch: 0,
        owner: alice.public().user_id(),
    };
    let (parents, clock) = log.prepare(&channel).unwrap();
    log.append(Event::new(
        &alice,
        channel,
        0,
        parents,
        clock,
        1,
        EventKind::MembershipChange,
        meta.encode_invitation(),
    ))
    .unwrap();
    let (parents, clock) = log.prepare(&channel).unwrap();
    log.append(Event::new(
        &alice,
        channel,
        1,
        parents,
        clock,
        1,
        EventKind::ChannelOwnerTransfer,
        bob.public().user_id().into_bytes(),
    ))
    .unwrap();
    let archived = archived_channel_summaries(&log, &ChannelBook::new());
    assert_eq!(archived[0].owner, alice.public().user_id());
    let (parents, clock) = log.prepare(&channel).unwrap();
    log.append(Event::new(
        &bob,
        channel,
        0,
        parents,
        clock,
        1,
        EventKind::ChannelAccept,
        vec![],
    ))
    .unwrap();
    let (parents, clock) = log.prepare(&channel).unwrap();
    log.append(Event::new(
        &alice,
        channel,
        2,
        parents,
        clock,
        1,
        EventKind::ChannelOwnerTransfer,
        bob.public().user_id().into_bytes(),
    ))
    .unwrap();
    assert_eq!(
        archived_channel_summaries(&log, &ChannelBook::new())[0].owner,
        bob.public().user_id()
    );
}

#[tokio::test]
async fn account_profile_propagates_signed_claims_over_loopback() {
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
        Account::from_secret_bytes_with_created_at(
            alice_acct.secret_bytes(),
            alice_acct.created_at_ms(),
        )
        .unwrap(),
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
        Account::from_secret_bytes_with_created_at(
            bob_acct.secret_bytes(),
            bob_acct.created_at_ms(),
        )
        .unwrap(),
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
        .save_account_profile_if("Alice".into(), "Self-declared bio".into(), |commit| {
            commit()
        })
        .await
        .unwrap();
    let mut got = None;
    for _ in 0..50 {
        got = bob_node.account_profile(&alice_acct.account_id());
        if got.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let card = got.expect("bob received signed trust card");
    assert!(card.verify());
    assert_eq!(card.bio, "Self-declared bio");
    assert_eq!(card.created_at_ms, alice_acct.created_at_ms());
    assert!(bob_node.account_profile(&bob_acct.account_id()).is_none());
    alice_node
        .save_account_profile_if("Alice updated".into(), "Updated bio".into(), |commit| {
            commit()
        })
        .await
        .unwrap();
    assert_eq!(
        alice_node
            .account_profile(&alice_acct.account_id())
            .unwrap()
            .created_at_ms,
        alice_acct.created_at_ms()
    );
}

#[tokio::test]
async fn account_profile_projection_retry_and_restart_preserve_avatar_and_identity_lifetime() {
    use crate::identity::account::Account;
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let open = || {
        personal_message_node(
            dir.path(),
            DeviceIdentity::from_secret_bytes(identity.secret_bytes().0, identity.secret_bytes().1),
            Account::from_secret_bytes_with_created_at(
                account.secret_bytes(),
                account.created_at_ms(),
            )
            .unwrap(),
        )
    };
    let node = open();
    assert!(node.account_profile(&account.account_id()).is_none());
    let card = crate::node::AccountProfile::new(
        &account,
        1,
        "Alice".into(),
        "bio".into(),
        Some(b"avatar".to_vec()),
    );
    node.account_profiles.fail_appends_for_test(true);
    assert!(node.receive_account_profile(&card).is_err());
    assert_eq!(
        node.account_profile(&account.account_id()),
        Some(card.clone())
    );
    node.account_profiles.fail_appends_for_test(false);
    node.receive_account_profile(&card).unwrap();
    assert_eq!(
        node.peer_avatar(&account.account_id()),
        Some(b"avatar".to_vec())
    );
    node.set_avatar(Some(b"edited".to_vec())).await.unwrap();
    assert_eq!(
        node.account_profile(&account.account_id()).unwrap().avatar,
        Some(b"edited".to_vec())
    );
    drop(node);
    let node = open();
    assert_eq!(
        node.account_profile(&account.account_id())
            .unwrap()
            .created_at_ms,
        account.created_at_ms()
    );
    assert_eq!(
        node.peer_avatar(&account.account_id()),
        Some(b"edited".to_vec())
    );
    drop(node);
    let other = Account::generate();
    // Account replacement resets account-bound delivery stores. Carry only the
    // public-card journal into the replacement profile to test its own lifetime.
    let replacement_dir = dir.path().join("replacement");
    std::fs::create_dir_all(&replacement_dir).unwrap();
    std::fs::copy(
        dir.path().join("account-profiles.log"),
        replacement_dir.join("account-profiles.log"),
    )
    .unwrap();
    let node = personal_message_node(
        &replacement_dir,
        DeviceIdentity::generate(),
        Account::from_secret_bytes_with_created_at(other.secret_bytes(), other.created_at_ms())
            .unwrap(),
    );
    assert!(node.account_profile(&other.account_id()).is_none());
    assert!(node.account_profiles.current().is_none());
}

#[tokio::test]
async fn account_profile_reconciles_newer_sibling_legacy_avatar() {
    use crate::identity::account::Account;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = personal_message_node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::from_secret_bytes_with_created_at(account.secret_bytes(), account.created_at_ms())
            .unwrap(),
    );
    node.save_account_profile_if("Alice".into(), "keep bio".into(), |commit| commit())
        .await
        .unwrap();
    let old = node.account_profile(&account.account_id()).unwrap();
    let legacy = crate::node::ProfilePayload::new(
        &account,
        Some(b"sibling avatar".to_vec()),
        old.revision + 1,
    );
    assert!(node.receive_legacy_profile(&legacy).unwrap());
    let card = node.account_profile(&account.account_id()).unwrap();
    assert_eq!(card.avatar, legacy.avatar);
    assert_eq!(card.bio, "keep bio");
    assert_eq!(card.created_at_ms, account.created_at_ms());
    assert!(card.revision > legacy.updated_at);
    assert!(card.verify());
}

#[tokio::test]
async fn account_profile_exhausted_avatar_revision_rejects_before_either_store_mutates() {
    use crate::identity::account::Account;
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = personal_message_node(
        dir.path(),
        DeviceIdentity::generate(),
        Account::from_secret_bytes_with_created_at(account.secret_bytes(), account.created_at_ms())
            .unwrap(),
    );
    let card = crate::node::AccountProfile::new(
        &account,
        u64::MAX,
        "Alice".into(),
        "bio".into(),
        Some(b"original".to_vec()),
    );
    node.receive_account_profile(&card).unwrap();
    let avatars_before = std::fs::read(dir.path().join("profiles.log")).unwrap();
    let cards_before = std::fs::read(dir.path().join("account-profiles.log")).unwrap();
    assert!(node.set_avatar(Some(b"edit".to_vec())).await.is_err());
    assert_eq!(node.peer_avatar(&account.account_id()), card.avatar);
    assert_eq!(node.account_profile(&account.account_id()), Some(card));
    assert_eq!(
        std::fs::read(dir.path().join("profiles.log")).unwrap(),
        avatars_before
    );
    assert_eq!(
        std::fs::read(dir.path().join("account-profiles.log")).unwrap(),
        cards_before
    );
}

#[tokio::test]
async fn account_profile_terminal_legacy_avatar_reconciles_on_restart() {
    use crate::identity::account::Account;
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let open = || {
        personal_message_node(
            dir.path(),
            DeviceIdentity::from_secret_bytes(identity.secret_bytes().0, identity.secret_bytes().1),
            Account::from_secret_bytes_with_created_at(
                account.secret_bytes(),
                account.created_at_ms(),
            )
            .unwrap(),
        )
    };
    let node = open();
    let card = crate::node::AccountProfile::new(
        &account,
        u64::MAX - 2,
        "Alice".into(),
        "bio".into(),
        Some(b"original".to_vec()),
    );
    node.receive_account_profile(&card).unwrap();
    let legacy =
        crate::node::ProfilePayload::new(&account, Some(b"terminal sibling".to_vec()), u64::MAX);
    assert!(node.account_profiles.receive(&legacy).unwrap());
    drop(node);
    let node = open();
    let card = node.account_profile(&account.account_id()).unwrap();
    assert_eq!(card.revision, u64::MAX);
    assert_eq!(card.avatar, legacy.avatar);
    assert_eq!(card.created_at_ms, account.created_at_ms());
    assert!(card.verify());
}

#[tokio::test]
async fn account_profile_linked_device_pair_preserves_metadata_and_requires_card_parent() {
    use crate::identity::account::Account;
    let alice = DeviceIdentity::generate();
    let alice_acct = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_acct = Account::from_secret_bytes_with_created_at(
        alice_acct.secret_bytes(),
        alice_acct.created_at_ms(),
    )
    .unwrap();
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
        Account::from_secret_bytes_with_created_at(
            alice_acct.secret_bytes(),
            alice_acct.created_at_ms(),
        )
        .unwrap(),
        alice_roster,
        a_dm,
        a_ch,
        a_f,
        &dir.path().join("a/events.log"),
        &dir.path().join("a/sent.log"),
        "pw",
    )
    .unwrap();
    let bob_node = Node::open_with_account(
        bob,
        Account::from_secret_bytes_with_created_at(
            bob_acct.secret_bytes(),
            bob_acct.created_at_ms(),
        )
        .unwrap(),
        bob_roster,
        b_dm,
        b_ch,
        b_f,
        &dir.path().join("b/events.log"),
        &dir.path().join("b/sent.log"),
        "pw",
    )
    .unwrap();

    tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

    for (node, peer) in [(&alice_node, &bob_node), (&bob_node, &alice_node)] {
        node.account_devices
            .lock()
            .unwrap()
            .insert(crate::node::account_devices::AccountDevice {
                public: peer.identity.public(),
                certificate: alice_acct.certify(&peer.identity.public().ed25519_pub),
            })
            .unwrap();
    }
    let old = crate::node::AccountProfile::new(
        &alice_acct,
        10,
        "Alice".into(),
        "old bio".into(),
        Some(b"old avatar".to_vec()),
    );
    bob_node.receive_account_profile(&old).unwrap();
    let latest = crate::node::AccountProfile::new(
        &alice_acct,
        13,
        "Alice".into(),
        "new bio".into(),
        Some(b"new avatar".to_vec()),
    );
    alice_node.receive_account_profile(&latest).unwrap();
    alice_node
        .save_account_profile_if("Alice".into(), "new bio".into(), |commit| commit())
        .await
        .unwrap();
    let received = bob_node.account_profile(&alice_acct.account_id()).unwrap();
    assert_eq!(received.bio, "new bio");
    assert_eq!(received.avatar, Some(b"new avatar".to_vec()));
    assert_eq!(
        received,
        alice_node
            .account_profile(&alice_acct.account_id())
            .unwrap()
    );

    let conv = dm_conversation_id(&alice_node.identity.public(), &bob_node.identity.public());
    let pair: Vec<_> = alice_node
        .log
        .lock()
        .unwrap()
        .events(&conv)
        .into_iter()
        .filter(|event| event.kind == EventKind::Profile)
        .cloned()
        .collect();
    assert_eq!(pair.len(), 2);
    assert!(pair[0].lamport < pair[1].lamport);
    assert_eq!(pair[1].parents, vec![pair[0].id]);
    let delayed = personal_message_node(
        &dir.path().join("delayed"),
        DeviceIdentity::generate(),
        Account::from_secret_bytes_with_created_at(
            alice_acct.secret_bytes(),
            alice_acct.created_at_ms(),
        )
        .unwrap(),
    );
    add_account_peer(
        &delayed.roster,
        &alice_node.identity,
        &alice_acct,
        "Alice",
        4000,
        &delayed.user_id(),
    );
    assert!(matches!(
        delayed.log.lock().unwrap().append(pair[1].clone()),
        Err(crate::eventlog::LogError::MissingParents(_))
    ));
    delayed.log.lock().unwrap().append(pair[0].clone()).unwrap();
    delayed.log.lock().unwrap().append(pair[1].clone()).unwrap();
    // The profile ciphertext is sealed to Bob, so admission proves causality;
    // Bob's real loopback session above proves decryption and metadata convergence.
}

#[tokio::test]
async fn channel_acceptance_returns_exact_ids_without_waiting_for_network_fanout() {
    let root = tempfile::tempdir().unwrap();
    let (dm, _) = mpsc::unbounded_channel();
    let (channel_tx, _) = mpsc::unbounded_channel();
    let (files, _) = mpsc::unbounded_channel();
    let node = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel_tx,
        files,
        &root.path().join("messages.log"),
        &root.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let channel = node.create_channel("Acceptance", vec![]).await.unwrap();
    let text = node
        .enqueue_channel_message_reply_expiring_if(channel, b"kk", None, None, |accept| accept())
        .unwrap();
    let sticker = node
        .enqueue_sticker_channel_expiring_if(channel, "heart", b"heart", None, |accept| accept())
        .unwrap();
    let path = root.path().join("photo.png");
    std::fs::write(&path, b"\x89PNG\r\n\x1a\nphoto").unwrap();
    let (photo, file) = node
        .enqueue_file_channel_progress_expiring_if(
            channel,
            &path,
            crate::file::FileKind::Media,
            |_| {},
            None,
            |accept| accept(),
        )
        .await
        .unwrap();
    let history = node.channel_history(channel, 10);
    assert_eq!(history.len(), 3);
    for id in [text, sticker, photo] {
        assert!(history.iter().any(|row| row.id == id));
    }
    assert!(node.file_progress(file).is_some());
}

#[tokio::test]
async fn send_authorization_never_retains_the_delivery_journal_lock() {
    let root = tempfile::tempdir().unwrap();
    let (dm, _) = mpsc::unbounded_channel();
    let (channel_tx, _) = mpsc::unbounded_channel();
    let (files, _) = mpsc::unbounded_channel();
    let node = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel_tx,
        files,
        &root.path().join("messages.log"),
        &root.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let channel = node.create_channel("Lock ordering", vec![]).await.unwrap();
    let authorize = |commit: &mut dyn FnMut() -> Result<(), NodeError>| {
        assert!(
            node.delivery.try_lock().is_ok(),
            "requesting host session authority while holding the delivery journal can deadlock signed-card publication"
        );
        commit()
    };
    node.enqueue_channel_message_reply_expiring_if(channel, b"channel", None, None, authorize)
        .unwrap();
    node.enqueue_to_account_if(&node.account_id(), b"self", None, authorize)
        .await
        .unwrap();
    let path = root.path().join("file.txt");
    std::fs::write(&path, b"file bytes").unwrap();
    node.enqueue_file_to_account_progress_expiring_if(
        &node.account_id(),
        &path,
        crate::file::FileKind::File,
        |_| {},
        None,
        authorize,
    )
    .await
    .unwrap();
    let request = OutgoingMessageRequest {
        request_id: [33; 32],
        destination: MessageDestination::Channel(channel),
        text: b"request".to_vec(),
        reply_to: None,
        expires_at_ms: None,
    };
    node.enqueue_message_once_if(&request, authorize)
        .await
        .unwrap();
    assert!(
        node.enqueue_message_once_if(&request, authorize)
            .await
            .unwrap()
            .duplicate
    );
}
