use super::super::tests::profile;
use super::*;
use crate::identity::{account::Account, device::DeviceIdentity};

#[test]
fn indexed_current_avatars_page_before_bodies_and_keep_clear_order() {
    let dir = tempfile::tempdir().unwrap();
    let owner = profile(
        dir.path(),
        &DeviceIdentity::generate(),
        &Account::generate(),
    );
    let mut store =
        IndexedProfileStore::from_profile(owner.clone(), &dir.path().join("avatars"), "pw")
            .unwrap();
    // Empty compatibility keys and the full stored u64 clock domain survive paging.
    assert!(store.record("", u64::MAX, None).unwrap());
    for n in 0..24 {
        assert!(
            store
                .record(
                    &format!("{n:032x}"),
                    1,
                    Some(vec![n as u8; crate::node::profile::MAX_AVATAR_BYTES])
                )
                .unwrap()
        );
    }
    assert!(store.record(&format!("{:032x}", 0), 1, None).unwrap());
    assert!(
        !store
            .record(&format!("{:032x}", 0), 1, Some(vec![0; 1]))
            .unwrap()
    );
    reset_body_reads();
    let page = store.page(None).unwrap();
    assert_eq!(page.rows.first().unwrap(), &(String::new(), None));
    assert!(page.rows.len() < 16);
    assert!(!page.complete);
    assert_eq!(body_reads(), page.rows.len());
    let mut count = 0;
    store
        .visit(|_, _, _| {
            count += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(count, 25);
    assert_eq!(store.updated_at("").unwrap(), Some(u64::MAX));
    let revision = store.revision().unwrap();
    owner.database().transaction(|c| {
        c.execute_batch("CREATE TEMP TRIGGER fail_avatar BEFORE INSERT ON current_avatar_bodies BEGIN SELECT RAISE(ABORT,'write failed'); END;")?; Ok(())
    }).unwrap();
    assert!(store.record("new", 2, None).is_err());
    assert_eq!(store.updated_at("new").unwrap(), None);
    assert_eq!(store.revision().unwrap(), revision);
}

#[test]
fn indexed_current_avatars_reject_overbudget_or_missing_body_before_loading() {
    let dir = tempfile::tempdir().unwrap();
    let owner = profile(
        dir.path(),
        &DeviceIdentity::generate(),
        &Account::generate(),
    );
    let mut store =
        IndexedProfileStore::from_profile(owner.clone(), &dir.path().join("avatars"), "pw")
            .unwrap();
    store.record("alice", 1, Some(vec![1; 32])).unwrap();
    owner
        .database()
        .transaction(|c| {
            c.execute(
                "UPDATE current_avatar_headers SET encoded_bytes=?1",
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
            c.execute("DELETE FROM current_avatar_bodies", [])?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        store.avatar("alice"),
        Err(LogError::CorruptFile(_))
    ));
    assert_eq!(body_reads(), 0);
}

#[test]
fn indexed_current_avatar_migration_failure_keeps_original_and_retries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("avatars");
    let mut old = ProfileStore::open(&path, "pw").unwrap();
    old.record("alice", 1, Some(vec![3; 128])).unwrap();
    drop(old);
    let original = std::fs::read(&path).unwrap();
    let owner = profile(
        dir.path(),
        &DeviceIdentity::generate(),
        &Account::generate(),
    );
    owner.database().transaction(|c| {
        c.execute_batch(SCHEMA)?;
        c.execute("INSERT INTO current_avatar_state(singleton) VALUES(1)",[])?;
        c.execute_batch("CREATE TEMP TRIGGER fail_avatar BEFORE INSERT ON current_avatar_bodies BEGIN SELECT RAISE(ABORT,'import failed'); END;")?;
        Ok(())
    }).unwrap();
    assert!(IndexedProfileStore::from_profile(owner.clone(), &path, "pw").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    owner
        .database()
        .transaction(|c| {
            c.execute_batch("DROP TRIGGER fail_avatar")?;
            Ok(())
        })
        .unwrap();
    let store = IndexedProfileStore::from_profile(owner.clone(), &path, "pw").unwrap();
    assert_eq!(store.avatar("alice").unwrap(), Some(vec![3; 128]));
    assert!(ProfileStore::open(&path, "pw").is_err());
    drop(store);
    owner.database().transaction(|c| {c.execute_batch("DROP TABLE current_avatar_bodies; DROP TABLE current_avatar_headers; DROP TABLE current_avatar_state;")?;Ok(())}).unwrap();
    assert!(
        IndexedProfileStore::from_profile(owner, &path, "pw").is_err(),
        "published missing authority cannot be recreated empty"
    );
}
