use super::*;
use crate::discovery::Roster;
use crate::identity::account::Account;
use std::sync::{Arc, Mutex};

fn open(path: &std::path::Path, account: &Account) -> Arc<Node> {
    let (dm, _) = tokio::sync::mpsc::unbounded_channel();
    let (channel, _) = tokio::sync::mpsc::unbounded_channel();
    let (file, _) = tokio::sync::mpsc::unbounded_channel();
    Node::open_with_account(
        crate::identity::keystore::load_or_create(&path.join("identity.keystore"), "pw").unwrap(),
        Account::from_secret_bytes_with_created_at(account.secret_bytes(), account.created_at_ms())
            .unwrap(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        file,
        &path.join("messages.log"),
        &path.join("sent.log"),
        "pw",
    )
    .unwrap()
}

#[test]
fn legacy_manual_dates_are_not_promoted_when_preserving_existing_profile_content() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::from_secret_bytes([9; 32]);
    let node = open(dir.path(), &account);
    let legacy = TrustCard::new(
        &account,
        1,
        "Old name".into(),
        "Existing bio".into(),
        Some(42),
        Some(b"photo".to_vec()),
    );
    node.receive_trust_card(&legacy).unwrap();
    node.set_profile_display_name("Current name").unwrap();
    let profile = node.account_profile(&account.account_id()).unwrap();
    assert_eq!(profile.created_at_ms, None);
    assert_eq!(profile.bio, "Existing bio");
    assert_eq!(profile.display_name, "Current name");
    assert_eq!(profile.avatar, legacy.avatar);
    drop(node);
    let reopened = open(dir.path(), &account);
    assert_eq!(
        reopened.account_profile(&account.account_id()),
        Some(profile)
    );
}

#[tokio::test]
async fn biography_edits_take_the_latest_nickname_at_commit_and_keep_creation_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    node.set_profile_display_name("Original").unwrap();
    node.save_account_bio_if("New bio".into(), |commit| {
        node.set_profile_display_name("Renamed while queued")?;
        commit()
    })
    .await
    .unwrap();
    let profile = node.account_profile(&account.account_id()).unwrap();
    assert_eq!(profile.display_name, "Renamed while queued");
    assert_eq!(profile.bio, "New bio");
    assert_eq!(profile.created_at_ms, account.created_at_ms());
}

#[tokio::test]
async fn rejected_bio_and_retired_authority_leave_the_saved_profile_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    node.set_profile_display_name("Alice").unwrap();
    let before = node.account_profile(&account.account_id());
    let bytes = std::fs::read(dir.path().join("account-profiles.log")).unwrap();
    assert!(
        node.save_account_bio_if("界".repeat(683), |commit| commit())
            .await
            .is_err()
    );
    assert!(
        node.save_account_bio_if("queued".into(), |_| Err(NodeError::Authorization(
            "retired".into()
        )))
        .await
        .is_err()
    );
    assert_eq!(node.account_profile(&account.account_id()), before);
    assert_eq!(
        std::fs::read(dir.path().join("account-profiles.log")).unwrap(),
        bytes
    );
}

#[test]
fn own_unknown_creation_metadata_cannot_be_filled_by_a_received_profile() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::from_secret_bytes([7; 32]);
    let node = open(dir.path(), &account);
    node.set_profile_display_name("Legacy").unwrap();
    let before = node.account_profile(&account.account_id()).unwrap();
    let mut contradictory = before.clone();
    contradictory.revision += 1;
    contradictory.created_at_ms = Some(42);
    contradictory.resign(&account);
    assert!(!node.receive_account_profile(&contradictory).unwrap());
    assert_eq!(node.account_profile(&account.account_id()), Some(before));
}

#[test]
fn legacy_bio_only_updates_invalidate_visible_profiles_without_an_avatar_change() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::from_secret_bytes([8; 32]);
    let node = open(dir.path(), &account);
    let mut events = node.take_profile_receiver().unwrap();
    for revision in [1, 2] {
        if revision == 2 {
            // A newer sibling compatibility avatar can already be projected.
            node.account_profiles
                .receive(&ProfilePayload::new(&account, None, 100))
                .unwrap();
        }
        let legacy = TrustCard::new(
            &account,
            revision,
            "Alice".into(),
            format!("bio {revision}"),
            None,
            None,
        );
        assert!(node.receive_trust_card(&legacy).unwrap());
        assert_eq!(
            events
                .try_recv()
                .expect("accepted biography must invalidate observers")
                .account_id,
            account.account_id()
        );
    }
}

#[tokio::test]
async fn first_bio_edit_initializes_a_missing_profile_without_losing_account_metadata() {
    for account in [Account::generate(), Account::from_secret_bytes([41; 32])] {
        let dir = tempfile::tempdir().unwrap();
        let node = open(dir.path(), &account);
        node.set_avatar(Some(b"existing avatar".to_vec()))
            .await
            .unwrap();
        assert!(node.account_profile(&account.account_id()).is_none());
        node.save_account_bio_with_initial_name_if(
            "Current account nickname".into(),
            "New bio".into(),
            |commit| commit(),
        )
        .await
        .unwrap();
        let profile = node.account_profile(&account.account_id()).unwrap();
        assert_eq!(profile.display_name, "Current account nickname");
        assert_eq!(profile.bio, "New bio");
        assert_eq!(profile.avatar, Some(b"existing avatar".to_vec()));
        assert_eq!(profile.created_at_ms, account.created_at_ms());
        assert!(profile.verify());
        let sibling_dir = tempfile::tempdir().unwrap();
        let sibling = open(sibling_dir.path(), &account);
        assert_ne!(node.user_id(), sibling.user_id());
        assert!(sibling.receive_account_profile(&profile).unwrap());
        assert_eq!(
            sibling.account_profile(&account.account_id()),
            Some(profile.clone())
        );
        drop(node);
        assert_eq!(
            open(dir.path(), &account).account_profile(&account.account_id()),
            Some(profile)
        );
    }
}

#[tokio::test]
async fn initial_bio_name_never_overwrites_a_profile_received_before_commit() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    node.save_account_bio_with_initial_name_if(
        "Stale local name".into(),
        "Edited bio".into(),
        |commit| {
            node.set_profile_display_name("Latest account nickname")?;
            commit()
        },
    )
    .await
    .unwrap();
    let profile = node.account_profile(&account.account_id()).unwrap();
    assert_eq!(profile.display_name, "Latest account nickname");
    assert_eq!(profile.bio, "Edited bio");
}

#[tokio::test]
async fn rejected_first_bio_edits_do_not_initialize_or_publish_a_profile() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    for (name, bio) in [("Name", "界".repeat(683)), ("", "Bio".into())] {
        assert!(
            node.save_account_bio_with_initial_name_if(name.into(), bio, |commit| commit(),)
                .await
                .is_err()
        );
        assert!(node.account_profile(&account.account_id()).is_none());
    }
    assert!(
        node.save_account_bio_with_initial_name_if("Name".into(), "Retired".into(), |_| Err(
            NodeError::Authorization("retired".into())
        ),)
            .await
            .is_err()
    );
    assert!(node.account_profile(&account.account_id()).is_none());
}

#[tokio::test]
async fn profile_publication_releases_store_before_channel_delivery() {
    for operation in ["background", "biography", "avatar", "legacy-avatar"] {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let node = open(dir.path(), &account);
        if operation != "legacy-avatar" {
            node.set_profile_display_name("Alice").unwrap();
        }
        let channel = node
            .create_channel("Concurrent publication", vec![])
            .await
            .unwrap();
        let peer_identity = crate::identity::device::DeviceIdentity::generate();
        let peer_account = Account::generate();
        // A closed loopback listener makes any post-acceptance delivery fail promptly.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let announce = crate::discovery::Announce::new_with_account(
            &peer_identity,
            &peer_account,
            "Bob",
            port,
        );
        node.roster.lock().unwrap().update(
            &announce,
            "127.0.0.1".parse().unwrap(),
            &node.user_id(),
        );
        let peer = node.profile_fanout_targets().pop().unwrap();
        let weak = Arc::downgrade(&node);
        let checked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let checked_hook = checked.clone();
        *node.profile_prepare_hook.lock().unwrap() = Some(Box::new(move || {
            let node = weak.upgrade().unwrap();
            assert!(
                node.profiles.try_lock().is_ok(),
                "profile delivery must release the store before waiting for channel state"
            );
            // Pause publication at its real channel-acquisition boundary while
            // another thread publishes inventory and accepts an ordinary send.
            let (finished, completion) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                node.publish_group_inventory(&mut None);
                assert!(!node.list_channels().is_empty());
                node.enqueue_channel_message_reply_expiring_if(
                    channel,
                    b"Concurrent channel message",
                    None,
                    None,
                    |commit| commit(),
                )
                .unwrap();
                finished.send(()).unwrap();
            });
            completion
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("inventory and channel send blocked behind profile publication");
            worker.join().unwrap();
            checked_hook.store(true, std::sync::atomic::Ordering::SeqCst);
        }));
        match operation {
            "background" => {
                assert!(node.prepare_current_profile_delivery(&peer).is_some());
            }
            "biography" => {
                node.save_account_bio_if("Updated bio".into(), |commit| commit())
                    .await
                    .unwrap();
            }
            _ => {
                node.set_avatar(Some(b"Updated avatar".to_vec()))
                    .await
                    .unwrap();
            }
        }
        assert!(
            checked.load(std::sync::atomic::Ordering::SeqCst),
            "{operation} did not prepare a profile"
        );
    }
}

#[tokio::test]
async fn group_inventory_releases_channels_before_profile_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    node.set_profile_display_name("Alice").unwrap();
    let channel = node
        .create_channel("Inventory lock order", vec![])
        .await
        .unwrap();
    let weak = Arc::downgrade(&node);
    let checked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let checked_hook = checked.clone();
    *node.profile_read_hook.lock().unwrap() = Some(Box::new(move || {
        let node = weak.upgrade().unwrap();
        assert!(
            node.channels.try_lock().is_ok(),
            "inventory must release channels before waiting for a signed profile"
        );
        checked_hook.store(true, std::sync::atomic::Ordering::SeqCst);
    }));
    node.publish_group_inventory(&mut None);
    assert!(checked.load(std::sync::atomic::Ordering::SeqCst));
    let references = node.group_inventory_references();
    let reference = references.get(&hex::encode(channel.as_bytes())).unwrap();
    assert!(
        reference
            .members
            .iter()
            .any(|member| member.name.as_deref() == Some("Alice"))
    );
}

#[test]
fn account_display_name_prefers_signed_profile_and_retains_offline_discovery_names() {
    use crate::discovery::announce::Announce;
    use crate::identity::device::DeviceIdentity;
    use std::{
        net::{IpAddr, Ipv4Addr},
        time::Duration,
    };
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), &Account::generate());
    let contact = Account::generate();
    let device = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&device, &contact, "Duncan", 47001);
    node.roster
        .lock()
        .unwrap()
        .update(&proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &node.user_id());
    assert_eq!(
        node.account_display_name(&contact.account_id()).as_deref(),
        Some("Duncan")
    );
    node.roster.lock().unwrap().evict_stale(Duration::ZERO);
    assert_eq!(
        node.account_display_name(&contact.account_id()).as_deref(),
        Some("Duncan")
    );
    let profile = AccountProfile::new(&contact, 1, "Duncan Jones".into(), String::new(), None);
    node.receive_account_profile(&profile).unwrap();
    assert_eq!(
        node.account_display_name(&contact.account_id()).as_deref(),
        Some("Duncan Jones")
    );
    assert_eq!(
        node.account_display_name(&Account::generate().account_id()),
        None
    );
}
