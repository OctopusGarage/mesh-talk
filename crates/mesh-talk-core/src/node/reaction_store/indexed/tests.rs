use super::*;
use crate::{
    identity::{account::Account, device::DeviceIdentity},
    storage::profile::ProfileStorageLease,
};
fn fixture(root: &Path, account: &Account) -> Arc<ProfileStorage> {
    let identity = DeviceIdentity::from_secret_bytes([21; 32], [22; 32]);
    let profile = ProfileStorage::open_participant(
        ProfileStorageLease::acquire(root).unwrap(),
        &identity.public(),
        &account.public(),
        &root.join("events"),
        &root.join("sent"),
        "pw",
    )
    .unwrap();
    profile.complete_participant("pw", None).unwrap();
    profile
}
fn payload(target: u8, remove: bool) -> ReactionPayload {
    ReactionPayload {
        target: EventId::new([target; 32]),
        emoji: "👍".into(),
        remove,
    }
}
fn conversation() -> ConversationId {
    ConversationId::new([1; 32])
}
#[test]
fn indexed_authored_reactions_pages_precede_bodies_and_keep_unsigned_remove_ties() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let key = account.public().ed25519_pub;
    let profile = fixture(dir.path(), &account);
    let path = dir.path().join("own-reactions");
    let mut store = IndexedReactionStore::from_profile(profile.clone(), &path, "pw", key).unwrap();
    for n in 0..130 {
        store
            .record(conversation(), u64::MAX, payload(n, false))
            .unwrap();
    }
    store
        .record(conversation(), u64::MAX, payload(0, true))
        .unwrap();
    store
        .record(conversation(), u64::MAX, payload(0, false))
        .unwrap();
    store
        .record(conversation(), u64::MAX - 1, payload(0, false))
        .unwrap();
    BODY_READS.with(|n| n.set(0));
    assert!(matches!(
        store.page(None, None, None, usize::MAX, 1),
        Err(LogError::InvalidPageBudget)
    ));
    assert!(matches!(
        store.page(None, None, None, 1, 1),
        Err(LogError::PageBudgetTooSmall { .. })
    ));
    assert_eq!(BODY_READS.with(|n| n.get()), 0);
    let first = store
        .page(
            Some(conversation()),
            Some(EventId::new([0; 32])),
            None,
            128,
            8 * 1024 * 1024,
        )
        .unwrap();
    assert_eq!(first.rows.len(), 1);
    assert!(first.rows[0].payload.remove);
    assert_eq!(first.rows[0].wall, u64::MAX);
    assert_eq!(BODY_READS.with(|n| n.get()), 1);
    let page = store.page(None, None, None, 128, 8 * 1024 * 1024).unwrap();
    assert_eq!(page.rows.len(), 128);
    assert_eq!(
        store
            .page(None, None, page.next, 128, 8 * 1024 * 1024)
            .unwrap()
            .rows
            .len(),
        2
    );
    drop(store);
    drop(profile);
    crate::storage::record_log::frame_observations::reset();
    BODY_READS.with(|n| n.set(0));
    let profile = fixture(dir.path(), &account);
    IndexedReactionStore::authenticate_legacy(&profile, &path, "pw", key).unwrap();
    let store = IndexedReactionStore::from_profile(profile, &path, "pw", key).unwrap();
    assert_eq!(store.len(), 130);
    assert_eq!(BODY_READS.with(|n| n.get()), 0);
    assert_eq!(
        crate::storage::record_log::frame_observations::maximum_capacity(),
        0
    );
}
#[test]
fn indexed_authored_reactions_scoped_publication_resumes_each_barrier_fault() {
    for fault in [
        "transcript-imported",
        "transcript-barrier",
        "transcript-published",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let key = account.public().ed25519_pub;
        let profile = fixture(dir.path(), &account);
        let path = dir.path().join("own-reactions");
        let mut legacy = super::super::ReactionStore::open(&path, "pw").unwrap();
        legacy.record(conversation(), 1, payload(1, false)).unwrap();
        drop(legacy);
        crate::node::transcript_repository::publication::fail_at(Some(fault));
        assert!(IndexedReactionStore::from_profile(profile.clone(), &path, "pw", key).is_err());
        crate::node::transcript_repository::publication::fail_at(None);
        crate::storage::record_log::frame_observations::reset();
        IndexedReactionStore::authenticate_legacy(&profile, &path, "pw", key).unwrap();
        let store = IndexedReactionStore::from_profile(profile.clone(), &path, "pw", key).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(
            crate::storage::record_log::frame_observations::maximum_capacity(),
            0
        );
        assert!(super::super::ReactionStore::open(&path, "pw").is_err());
        assert!(
            PublishedHistorySource::open(
                profile.clone(),
                &path,
                "pw",
                AUTHORED_REACTIONS,
                |_, _, _| Ok(())
            )
            .is_err(),
            "account-qualified source must not become unscoped"
        );
        let other = Account::generate().public().ed25519_pub;
        let second = dir.path().join("other-account-reactions");
        let mut legacy = super::super::ReactionStore::open(&second, "pw").unwrap();
        legacy.record(conversation(), 2, payload(2, false)).unwrap();
        drop(legacy);
        let next =
            IndexedReactionStore::from_profile(profile.clone(), &second, "pw", other).unwrap();
        assert_eq!(next.len(), 1);
        assert!(IndexedReactionStore::from_profile(profile.clone(), &second, "pw", key).is_err());
        assert!(IndexedReactionStore::from_profile(profile.clone(), &path, "pw", other).is_err());
        profile
            .database()
            .transaction(|c| {
                c.execute(
                    "UPDATE participant_profile SET account=?1",
                    [other.as_slice()],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(matches!(
            store.page(None, None, None, 128, 8 * 1024 * 1024),
            Err(LogError::StaleCursor)
        ));
        assert_eq!(
            next.page(None, None, None, 128, 8 * 1024 * 1024)
                .unwrap()
                .rows[0]
                .payload
                .target,
            EventId::new([2; 32])
        );
    }
}
#[test]
fn indexed_authored_reactions_import_failure_preserves_source_and_missing_authority_fails() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let key = account.public().ed25519_pub;
    let profile = fixture(dir.path(), &account);
    let path = dir.path().join("own-reactions");
    let mut legacy = super::super::ReactionStore::open(&path, "pw").unwrap();
    legacy.record(conversation(), 1, payload(1, false)).unwrap();
    drop(legacy);
    let original = std::fs::read(&path).unwrap();
    profile.database().transaction(|c|{c.execute_batch(SCHEMA)?;c.execute_batch("CREATE TEMP TRIGGER reject_reaction BEFORE INSERT ON authored_reaction_bodies BEGIN SELECT RAISE(ABORT,'injected'); END;")?;Ok(())}).unwrap();
    assert!(IndexedReactionStore::from_profile(profile.clone(), &path, "pw", key).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    profile
        .database()
        .transaction(|c| {
            c.execute_batch("DROP TRIGGER reject_reaction")?;
            Ok(())
        })
        .unwrap();
    let store = IndexedReactionStore::from_profile(profile.clone(), &path, "pw", key).unwrap();
    assert_eq!(store.len(), 1);
    drop(store);
    profile.database().transaction(|c|{c.execute_batch("DROP TABLE authored_reaction_bodies;DROP TABLE authored_reaction_keys;DROP TABLE authored_reaction_state;")?;Ok(())}).unwrap();
    assert!(IndexedReactionStore::from_profile(profile, &path, "pw", key).is_err());
}
#[test]
fn indexed_authored_reactions_write_rollback_and_removal_predicates_release_connection() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let key = account.public().ed25519_pub;
    let profile = fixture(dir.path(), &account);
    let path = dir.path().join("own-reactions");
    let mut store = IndexedReactionStore::from_profile(profile.clone(), &path, "pw", key).unwrap();
    store.record(conversation(), 1, payload(1, false)).unwrap();
    profile.database().transaction(|c|{c.execute_batch("CREATE TEMP TRIGGER reject_reaction BEFORE INSERT ON authored_reaction_bodies BEGIN SELECT RAISE(ABORT,'injected'); END;")?;Ok(())}).unwrap();
    assert!(store.record(conversation(), 2, payload(1, true)).is_err());
    assert!(
        !store.page(None, None, None, 1, 1024).unwrap().rows[0]
            .payload
            .remove
    );
    profile
        .database()
        .transaction(|c| {
            c.execute_batch("DROP TRIGGER reject_reaction")?;
            Ok(())
        })
        .unwrap();
    store
        .try_remove_where(|_, _, _| {
            profile
                .database()
                .read(|c| Ok(c.query_row("SELECT 1", [], |r| r.get::<_, bool>(0))?))
        })
        .unwrap();
    assert_eq!(store.len(), 0);
}
