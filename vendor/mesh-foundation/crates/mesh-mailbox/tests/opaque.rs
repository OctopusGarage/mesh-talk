#![cfg(feature = "spool")]
use mesh_mailbox::{
    Record, ScopeId,
    capacity::CapacityLimits,
    ports::MailboxStore,
    spool::{Deposit, Spool},
};
use mesh_storage::EncryptionKey;
use std::num::NonZeroU64;

#[test]
fn opaque_forum_records_reopen_without_chat_identities_or_events() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("opaque");
    let scope = ScopeId::new([7; 32]);
    let limits = CapacityLimits::new(2, 0, 4096, 0).unwrap();
    let open = || {
        Spool::open_with_key_provider(&path, scope, limits, NonZeroU64::new(10).unwrap(), |_| {
            Ok(EncryptionKey::from_derived_key([5; 32]))
        })
        .unwrap()
    };
    let first = Record::new(scope, b"forum encrypted notification".to_vec());
    let second = Record::new(scope, b"document collaboration update".to_vec());
    let prior;
    {
        let mut spool = open();
        assert_eq!(spool.deposit(first.clone(), 10).unwrap(), Deposit::Stored);
        prior = spool.view(11).delta(&scope, None, 1).unwrap().cursor;
        assert_eq!(spool.deposit(second.clone(), 11).unwrap(), Deposit::Stored);
    }
    let mut spool = open();
    let delta = spool.view(12).delta(&scope, Some(&prior), 1).unwrap();
    assert_eq!(delta.ids, [second.id]);
    let bodies = spool
        .view(12)
        .bodies(&scope, &delta.cursor, &delta.ids, 4096)
        .unwrap();
    assert_eq!(bodies, [second]);
    assert!(spool.expire_at(20).unwrap());
    assert!(
        spool
            .view(20)
            .bodies(&scope, &prior, &[first.id], 4096)
            .is_err()
    );
}
