use super::super::{AccountProfileStore, tests::profile};
use super::*;
use crate::identity::{account::Account, device::DeviceIdentity};
fn value(account: &Account, revision: u64, avatar: Option<Vec<u8>>) -> AccountProfile {
    AccountProfile::new(account, revision, "Person".into(), String::new(), avatar)
}
#[test]
fn indexed_signed_profiles_preserve_ties_dates_and_atomic_current_value() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profiles");
    let account = Account::from_secret_bytes([3; 32]);
    let id = account.public().account_id();
    let mut old = AccountProfileStore::open(&path, "pw").unwrap();
    let photo = value(&account, 7, Some(vec![3; 128]));
    let clear = value(&account, 7, None);
    assert!(old.record(photo.clone()).unwrap());
    assert!(old.record(clear.clone()).unwrap());
    drop(old);
    let owner = profile(
        dir.path(),
        &DeviceIdentity::generate(),
        &Account::generate(),
    );
    let mut store = IndexedAccountProfileStore::from_profile(owner.clone(), &path, "pw").unwrap();
    assert_eq!(store.get(&id).unwrap(), Some(clear));
    assert!(!store.accepts(&photo).unwrap());
    let mut dated = value(&account, 8, None);
    dated.created_at_ms = Some(42);
    dated.resign(&account);
    assert!(store.record(dated.clone()).unwrap());
    let mut changed = dated.clone();
    changed.revision = 9;
    changed.created_at_ms = Some(43);
    changed.resign(&account);
    assert!(!store.record(changed).unwrap());
    let mut bad = dated.clone();
    bad.revision = 10;
    assert!(!store.record(bad).unwrap());
    let revision = store.revision().unwrap();
    owner.database().transaction(|c| {c.execute_batch("CREATE TEMP TRIGGER fail_current BEFORE INSERT ON current_account_profile_bodies BEGIN SELECT RAISE(ABORT,'failed'); END;")?;Ok(())}).unwrap();
    let mut next = dated.clone();
    next.revision = 11;
    next.resign(&account);
    assert!(store.record(next).is_err());
    assert_eq!(store.get(&id).unwrap(), Some(dated));
    assert_eq!(store.revision().unwrap(), revision);
}
#[test]
fn indexed_signed_profiles_page_bytes_and_reject_missing_body_before_hydration() {
    let dir = tempfile::tempdir().unwrap();
    let owner = profile(
        dir.path(),
        &DeviceIdentity::generate(),
        &Account::generate(),
    );
    let mut store =
        IndexedAccountProfileStore::from_profile(owner.clone(), &dir.path().join("profiles"), "pw")
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
                "UPDATE current_account_profile_headers SET encoded_bytes=?1",
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
                "DELETE FROM current_account_profile_bodies WHERE account=?1",
                [&id],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(store.get(&id), Err(LogError::CorruptFile(_))));
    assert_eq!(body_reads(), 0);
}
#[test]
fn indexed_signed_profile_wire_quota_keeps_existing_value_replaceable() {
    let dir = tempfile::tempdir().unwrap();
    let owner = profile(
        dir.path(),
        &DeviceIdentity::generate(),
        &Account::generate(),
    );
    let mut store =
        IndexedAccountProfileStore::from_profile(owner.clone(), &dir.path().join("profiles"), "pw")
            .unwrap();
    let mut accepted = 0;
    for n in 1u32..=256 {
        let mut seed = [7; 32];
        seed[..4].copy_from_slice(&n.to_be_bytes());
        accepted += usize::from(
            store
                .record(value(
                    &Account::from_secret_bytes(seed),
                    1,
                    Some(vec![1; super::super::MAX_AVATAR_BYTES]),
                ))
                .unwrap(),
        );
    }
    assert_eq!(
        accepted, 255,
        "existing 32 MiB wire-byte cap includes signed metadata"
    );
    let mut seed = [7; 32];
    seed[..4].copy_from_slice(&1u32.to_be_bytes());
    let account = Account::from_secret_bytes(seed);
    assert!(store.record(value(&account, 2, None)).unwrap());
    let mut seed = [7; 32];
    seed[..4].copy_from_slice(&256u32.to_be_bytes());
    assert!(
        store
            .record(value(
                &Account::from_secret_bytes(seed),
                2,
                Some(vec![1; super::super::MAX_AVATAR_BYTES])
            ))
            .unwrap()
    );
    let (_, count, bytes) = owner.database().read(state_on).unwrap();
    assert_eq!(count, 256);
    assert!(bytes <= MAX_STORE_BYTES);
}

#[test]
fn indexed_profile_projection_ack_is_exact_and_names_need_no_payload() {
    let dir = tempfile::tempdir().unwrap();
    let owner = profile(
        dir.path(),
        &DeviceIdentity::generate(),
        &Account::generate(),
    );
    let mut store =
        IndexedAccountProfileStore::from_profile(owner, &dir.path().join("profiles"), "pw")
            .unwrap();
    let account = Account::generate();
    let id = account.account_id();
    store
        .record(value(&account, 1, Some(vec![7; 8192])))
        .unwrap();
    let old = store.projection_revision(&id).unwrap().unwrap();
    store.record(value(&account, 2, None)).unwrap();
    let current = store.projection_revision(&id).unwrap().unwrap();
    assert_ne!(old, current);
    reset_body_reads();
    store.acknowledge_projection(&id, old).unwrap();
    assert_eq!(
        store.pending_projection().unwrap(),
        vec![(id.clone(), current)]
    );
    assert_eq!(
        store.names_page(None).unwrap(),
        vec![(id.clone(), "Person".into())]
    );
    assert_eq!(store.version(&id).unwrap(), Some(2));
    assert_eq!(body_reads(), 0);
    store.acknowledge_projection(&id, current).unwrap();
    assert!(store.pending_projection().unwrap().is_empty());
}
