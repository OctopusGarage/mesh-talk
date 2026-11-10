use super::super::{TrustCardStore, tests::profile};
use super::*;
use crate::identity::{account::Account, device::DeviceIdentity};
fn value(account: &Account, revision: u64, avatar: Option<Vec<u8>>) -> TrustCard {
    TrustCard::new(
        account,
        revision,
        "Person".into(),
        String::new(),
        Some(u64::MAX),
        avatar,
    )
}
#[test]
fn indexed_trust_cards_keep_first_equal_revision_and_creation_authority() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cards");
    let account = Account::from_secret_bytes([4; 32]);
    let id = account.public().account_id();
    let first = value(&account, 4, Some(vec![3; 128]));
    let clear = value(&account, 4, None);
    let mut old = TrustCardStore::open(&path, "pw").unwrap();
    assert!(old.record(first.clone()).unwrap());
    assert!(!old.record(clear.clone()).unwrap());
    drop(old);
    let owner = profile(
        dir.path(),
        &DeviceIdentity::generate(),
        &Account::generate(),
    );
    let mut store = IndexedTrustCardStore::from_profile(owner.clone(), &path, "pw").unwrap();
    assert_eq!(store.get(&id).unwrap(), Some(first.clone()));
    assert!(!store.accepts(&clear).unwrap());
    let mut changed = value(&account, 5, None);
    changed.created_at_ms = Some(42);
    changed.resign(&account);
    assert!(!store.record(changed).unwrap());
    let mut invalid = value(&account, 7, None);
    invalid.display_name = "tampered".into();
    assert!(!store.record(invalid).unwrap());
    let revision = store.revision().unwrap();
    owner.database().transaction(|c| {c.execute_batch("CREATE TEMP TRIGGER fail_card BEFORE INSERT ON current_trust_card_bodies BEGIN SELECT RAISE(ABORT,'failed'); END;")?;Ok(())}).unwrap();
    assert!(store.record(value(&account, 8, None)).is_err());
    assert_eq!(store.revision().unwrap(), revision);
    assert_eq!(store.get(&id).unwrap(), Some(first));
}
#[test]
fn indexed_trust_card_page_selects_metadata_before_payloads() {
    let dir = tempfile::tempdir().unwrap();
    let owner = profile(
        dir.path(),
        &DeviceIdentity::generate(),
        &Account::generate(),
    );
    let mut store =
        IndexedTrustCardStore::from_profile(owner.clone(), &dir.path().join("cards"), "pw")
            .unwrap();
    for n in 1..=20 {
        store
            .record(value(
                &Account::from_secret_bytes([n; 32]),
                1,
                Some(vec![7; super::super::MAX_AVATAR_BYTES]),
            ))
            .unwrap();
    }
    reset_body_reads();
    let page = store.page(None).unwrap();
    assert!(page.rows.len() < 16);
    assert!(!page.complete);
    assert_eq!(body_reads(), page.rows.len());
    let mut count = 0;
    store
        .visit(|_| {
            count += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(count, 20);
    let id = page.rows[0].account_id();
    owner
        .database()
        .transaction(|c| {
            c.execute(
                "UPDATE current_trust_card_headers SET encoded_bytes=?1",
                [2 * 1024 * 1024],
            )?;
            Ok(())
        })
        .unwrap();
    reset_body_reads();
    assert!(matches!(
        store.page(None),
        Err(LogError::PageBudgetTooSmall { .. })
    ));
    assert_eq!(body_reads(), 0);
    owner
        .database()
        .transaction(|c| {
            c.execute(
                "DELETE FROM current_trust_card_bodies WHERE account=?1",
                [&id],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(store.get(&id), Err(LogError::CorruptFile(_))));
    assert_eq!(body_reads(), 0);
}
#[test]
fn indexed_trust_card_account_quota_allows_existing_account_updates() {
    let dir = tempfile::tempdir().unwrap();
    let owner = profile(
        dir.path(),
        &DeviceIdentity::generate(),
        &Account::generate(),
    );
    let mut store =
        IndexedTrustCardStore::from_profile(owner, &dir.path().join("cards"), "pw").unwrap();
    for n in 0u32..1024 {
        let mut seed = [9; 32];
        seed[..4].copy_from_slice(&n.to_be_bytes());
        assert!(
            store
                .record(value(&Account::from_secret_bytes(seed), 1, None))
                .unwrap()
        );
    }
    let mut seed = [9; 32];
    seed[..4].copy_from_slice(&1024u32.to_be_bytes());
    assert!(
        !store
            .record(value(&Account::from_secret_bytes(seed), 1, None))
            .unwrap()
    );
    seed[..4].copy_from_slice(&0u32.to_be_bytes());
    assert!(
        store
            .record(value(&Account::from_secret_bytes(seed), 2, None))
            .unwrap()
    );
}
