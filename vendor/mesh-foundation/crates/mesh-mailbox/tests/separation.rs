#![cfg(feature = "spool")]
use mesh_mailbox::{
    Record, RecordId, ScopeId,
    capacity::CapacityLimits,
    client::{MailboxTurnLimits, sync_turn},
    model::{MailboxCursor, MailboxDeltaPage},
    ports::{MailboxStore, MailboxTransport},
    spool::{Deposit, Retention, Spool, SpoolOptions},
};
use mesh_replication::pull::PullTurnLimits;
use mesh_storage::EncryptionKey;
struct Local<'a>(&'a Spool);
impl MailboxTransport for Local<'_> {
    type Error = mesh_mailbox::spool::SpoolError;
    async fn delta(
        &mut self,
        scope: ScopeId,
        prior: Option<&MailboxCursor>,
        limit: usize,
        max_bytes: usize,
    ) -> Result<MailboxDeltaPage, Self::Error> {
        self.0
            .view(1000)
            .delta_bounded(&scope, prior, limit, max_bytes)
    }
    async fn bodies(
        &mut self,
        scope: ScopeId,
        cursor: &MailboxCursor,
        ids: &[RecordId],
        max_bytes: usize,
    ) -> Result<Vec<Record>, Self::Error> {
        self.0.view(1000).bodies(&scope, cursor, ids, max_bytes)
    }
}
fn options() -> SpoolOptions {
    SpoolOptions {
        capacity: CapacityLimits::new(300, 0, 4 * 1024 * 1024, 0).unwrap(),
        retention: Retention::Manual,
        max_record_bytes: 512 * 1024,
        max_query_records: 256,
    }
}
#[tokio::test]
async fn large_records_and_small_turns_do_not_depend_on_wire_profile() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("opaque");
    let scope = ScopeId::new([3; 32]);
    let open = || {
        Spool::open_configured(&path, scope, options(), |_| {
            Ok(EncryptionKey::from_derived_key([7; 32]))
        })
        .unwrap()
    };
    let large = Record::new(scope, vec![5; 128 * 1024]);
    assert!(large.verify_integrity());
    let mut spool = open();
    assert_eq!(spool.deposit(large.clone(), 1).unwrap(), Deposit::Stored);
    assert!(!spool.expire_at(1000).unwrap());
    drop(spool);
    let spool = open();
    let mut transport = Local(&spool);
    let mut saved = vec![];
    let p = sync_turn(
        &mut transport,
        scope,
        None,
        MailboxTurnLimits::default(),
        |records, _| {
            saved.extend_from_slice(records);
            Ok::<_, ()>(())
        },
    )
    .await
    .unwrap();
    assert!(p.work.complete);
    assert_eq!(saved, [large]);
    assert_eq!(p.work.bytes, 128 * 1024);
}
#[tokio::test]
async fn two_hundred_records_and_a_one_kib_turn_are_exact() {
    let dir = tempfile::tempdir().unwrap();
    let scope = ScopeId::new([4; 32]);
    let mut spool = Spool::open_configured(&dir.path().join("s"), scope, options(), |_| {
        Ok(EncryptionKey::from_derived_key([8; 32]))
    })
    .unwrap();
    for i in 0..201u64 {
        spool
            .deposit(Record::new(scope, i.to_le_bytes().to_vec()), 1)
            .unwrap();
    }
    let mut transport = Local(&spool);
    let mut count = 0;
    let limits = MailboxTurnLimits {
        work: PullTurnLimits {
            records: 1,
            bytes: 1024,
            requests: 2,
        },
        pages: 1,
        page_records: 1,
    };
    let first = sync_turn(&mut transport, scope, None, limits, |r, _| {
        count += r.len();
        Ok::<_, ()>(())
    })
    .await
    .unwrap();
    assert_eq!(first.work.committed, 1);
    assert_eq!(first.work.bytes, 8);
    assert!(!first.work.complete);
    let next = sync_turn(
        &mut transport,
        scope,
        first.cursor,
        MailboxTurnLimits::default(),
        |r, _| {
            count += r.len();
            Ok::<_, ()>(())
        },
    )
    .await
    .unwrap();
    assert_eq!(next.work.committed, 200);
    assert!(next.work.complete);
    assert_eq!(count, 201);
    assert_eq!(next.work.requests, 8);
}

#[tokio::test]
async fn first_append_to_an_empty_source_is_not_a_retention_reset() {
    let dir = tempfile::tempdir().unwrap();
    let scope = ScopeId::new([9; 32]);
    let mut spool = Spool::open_configured(&dir.path().join("s"), scope, options(), |_| {
        Ok(EncryptionKey::from_derived_key([8; 32]))
    })
    .unwrap();
    let first = sync_turn(
        &mut Local(&spool),
        scope,
        None,
        MailboxTurnLimits::default(),
        |r, _| {
            assert!(r.is_empty());
            Ok::<_, ()>(())
        },
    )
    .await
    .unwrap();
    assert!(first.work.complete);
    spool.deposit(Record::new(scope, vec![1]), 10).unwrap();
    let next = sync_turn(
        &mut Local(&spool),
        scope,
        first.cursor,
        MailboxTurnLimits::default(),
        |_, _| Ok::<_, ()>(()),
    )
    .await
    .unwrap();
    assert_eq!(next.work.committed, 1);
    assert!(next.work.complete);
}

#[tokio::test]
async fn a_page_of_large_records_resumes_at_the_byte_budget() {
    let dir = tempfile::tempdir().unwrap();
    let scope = ScopeId::new([8; 32]);
    let mut spool = Spool::open_configured(&dir.path().join("s"), scope, options(), |_| {
        Ok(EncryptionKey::from_derived_key([8; 32]))
    })
    .unwrap();
    for i in 0..20u8 {
        spool
            .deposit(Record::new(scope, vec![i; 64 * 1024]), 1)
            .unwrap();
    }
    let first = sync_turn(
        &mut Local(&spool),
        scope,
        None,
        MailboxTurnLimits::default(),
        |_, _| Ok::<_, ()>(()),
    )
    .await
    .unwrap();
    assert_eq!(first.work.committed, 16);
    assert_eq!(first.work.bytes, 1024 * 1024);
    assert!(!first.work.complete);
    let next = sync_turn(
        &mut Local(&spool),
        scope,
        first.cursor,
        MailboxTurnLimits::default(),
        |_, _| Ok::<_, ()>(()),
    )
    .await
    .unwrap();
    assert_eq!(next.work.committed, 4);
    assert!(next.work.complete);
}
