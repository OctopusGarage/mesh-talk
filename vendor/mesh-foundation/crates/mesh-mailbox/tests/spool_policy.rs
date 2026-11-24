#![cfg(feature = "spool")]
use mesh_mailbox::{
    Record, ScopeId,
    capacity::CapacityLimits,
    ports::MailboxStore,
    spool::{Deposit, Retention, Spool, SpoolOptions},
};
use mesh_storage::EncryptionKey;
use std::num::NonZeroU64;
fn options(retention: Retention) -> SpoolOptions {
    SpoolOptions {
        capacity: CapacityLimits::new(2, 0, 4 * 1024 * 1024, 0).unwrap(),
        retention,
        max_record_bytes: 512 * 1024,
        max_query_records: 2,
    }
}
#[test]
fn manual_large_records_reopen_and_explicit_clear_invalidates_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("records");
    let scope = ScopeId::new([7; 32]);
    let open = || {
        Spool::open_configured(&path, scope, options(Retention::Manual), |_| {
            Ok(EncryptionKey::from_derived_key([3; 32]))
        })
        .unwrap()
    };
    let record = Record::new(scope, vec![9; 128 * 1024]);
    let cursor;
    {
        let mut spool = open();
        assert_eq!(spool.deposit(record.clone(), 1).unwrap(), Deposit::Stored);
        cursor = spool.view(999999).delta(&scope, None, 2).unwrap().cursor;
        assert!(!spool.expire_at(u64::MAX).unwrap());
    }
    let mut spool = open();
    assert_eq!(
        spool
            .view(999999)
            .bodies(&scope, &cursor, &[record.id], 128 * 1024)
            .unwrap(),
        [record]
    );
    assert!(spool.view(1).delta(&scope, None, 3).is_err());
    spool.clear().unwrap();
    assert!(
        spool
            .view(1)
            .bodies(&scope, &cursor, &cursor_ids(), 1024)
            .is_err()
    );
}
fn cursor_ids() -> Vec<mesh_mailbox::RecordId> {
    vec![]
}
#[test]
fn window_expiry_and_limits_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("records");
    let scope = ScopeId::new([1; 32]);
    let mut spool = Spool::open_configured(
        &path,
        scope,
        options(Retention::WindowAge(NonZeroU64::new(10).unwrap())),
        |_| Ok(EncryptionKey::from_derived_key([1; 32])),
    )
    .unwrap();
    let record = Record::new(scope, vec![1]);
    spool.deposit(record.clone(), 10).unwrap();
    let cursor = spool.view(11).delta(&scope, None, 1).unwrap().cursor;
    assert!(!spool.expire_at(19).unwrap());
    assert!(spool.expire_at(20).unwrap());
    assert!(
        spool
            .view(20)
            .bodies(&scope, &cursor, &[record.id], 10)
            .is_err()
    );
    assert_eq!(
        spool
            .deposit(Record::new(scope, vec![0; 512 * 1024 + 1]), 21)
            .unwrap(),
        Deposit::TooLarge
    );
    let mut bad = Record::new(scope, vec![1]);
    bad.body[0] = 2;
    assert!(spool.deposit(bad, 21).is_err());
    assert_eq!(spool.usage().envelopes, 0);
}
#[test]
fn invalid_config_does_not_touch_disk_and_writer_lease_is_exclusive() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new").join("records");
    let scope = ScopeId::new([1; 32]);
    let mut query_invalid = options(Retention::Manual);
    query_invalid.max_query_records = usize::MAX;
    assert!(
        Spool::open_configured(&path, scope, query_invalid, |_| Ok(
            EncryptionKey::from_derived_key([1; 32])
        ))
        .is_err()
    );
    assert!(!path.parent().unwrap().exists());
    let mut invalid = options(Retention::Manual);
    invalid.max_record_bytes = usize::MAX;
    assert!(
        Spool::open_configured(&path, scope, invalid, |_| Ok(
            EncryptionKey::from_derived_key([1; 32])
        ))
        .is_err()
    );
    assert!(!path.parent().unwrap().exists());
    let open = || {
        Spool::open_configured(&path, scope, options(Retention::Manual), |_| {
            Ok(EncryptionKey::from_derived_key([1; 32]))
        })
    };
    let lease = open().unwrap();
    assert!(open().is_err());
    drop(lease);
    assert!(open().is_ok());
}
#[test]
fn quotas_and_replay_record_limits_remain_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("records");
    let scope = ScopeId::new([8; 32]);
    {
        let mut spool = Spool::open_configured(&path, scope, options(Retention::Manual), |_| {
            Ok(EncryptionKey::from_derived_key([1; 32]))
        })
        .unwrap();
        for n in 0..2 {
            assert_eq!(
                spool.deposit(Record::new(scope, vec![n; 100]), 1).unwrap(),
                Deposit::Stored
            );
        }
        assert_eq!(
            spool.deposit(Record::new(scope, vec![3; 100]), 1).unwrap(),
            Deposit::CapacityUnavailable
        );
        assert_eq!(spool.usage().envelopes, 2);
    }
    let mut tighter = options(Retention::Manual);
    tighter.max_record_bytes = 50;
    assert!(
        Spool::open_configured(&path, scope, tighter, |_| Ok(
            EncryptionKey::from_derived_key([1; 32])
        ))
        .is_err()
    );
}

#[test]
fn existing_neutral_spool_bytes_survive_cursor_policy_separation() {
    use mesh_storage::record_log::EncryptedRecordLog;
    use serde::{Deserialize, Serialize};
    // Independent historical journal layout: cursor DTOs were never persisted.
    #[derive(Serialize, Deserialize)]
    enum OldJournal {
        Header {
            scope: [u8; 32],
            store_uuid: [u8; 16],
            epoch: i64,
        },
        Envelope {
            first_seen: u64,
            event: OldRecord,
        },
    }
    #[derive(Serialize, Deserialize)]
    struct OldRecord {
        id: [u8; 32],
        scope: [u8; 32],
        body: Vec<u8>,
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old");
    let scope = ScopeId::new([7; 32]);
    let record = Record::new(scope, b"existing bytes".to_vec());
    let derive = |_: &[u8; 16]| Ok(EncryptionKey::from_derived_key([3; 32]));
    {
        let mut log = EncryptedRecordLog::<OldJournal>::open_with_key_and_frame_limit(
            &path,
            b"MFMSP1",
            derive,
            128 * 1024,
            |_| Ok(()),
        )
        .unwrap();
        log.append_durable(&OldJournal::Header {
            scope: *scope.as_bytes(),
            store_uuid: [1; 16],
            epoch: 2,
        })
        .unwrap();
        log.append_durable(&OldJournal::Envelope {
            first_seen: 10,
            event: OldRecord {
                id: *record.id.as_bytes(),
                scope: *scope.as_bytes(),
                body: record.body.clone(),
            },
        })
        .unwrap();
    }
    let spool = Spool::open_configured(&path, scope, options(Retention::Manual), derive).unwrap();
    let page = spool.view(100).delta(&scope, None, 1).unwrap();
    assert_eq!(page.cursor.epoch, 2);
    assert_eq!(
        spool
            .view(100)
            .bodies(&scope, &page.cursor, &page.ids, 100)
            .unwrap(),
        [record]
    );
}
