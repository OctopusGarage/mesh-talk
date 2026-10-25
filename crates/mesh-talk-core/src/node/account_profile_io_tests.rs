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
