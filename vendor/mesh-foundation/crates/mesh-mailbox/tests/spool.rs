use mesh_identity::device::DeviceIdentity;
use mesh_mailbox::{
    RelayMailbox,
    capacity::CapacityLimits,
    rpc::{Frame, decode_frame, encode_frame, make_get_request},
    service::{MailboxStore, dispatch},
    spool::{Deposit, Spool, SpoolError},
};
use mesh_replication::legacy_event::{ConversationId, Event, EventKind};
use std::num::NonZeroU64;

#[test]
fn durable_spool_reopens_resumes_and_rejects_stale_bodies() {
    let path = tempfile::tempdir().unwrap();
    let file = path.path().join("mailbox");
    let source = DeviceIdentity::generate();
    let reader = DeviceIdentity::generate();
    let mailbox = RelayMailbox::federated(&source, &reader.public()).unwrap();
    let scope = mailbox.id();
    let wrap = |seq| {
        mailbox
            .wrap(&Event::new(
                &source,
                ConversationId::new([3; 32]),
                seq,
                vec![],
                seq,
                0,
                EventKind::Message,
                vec![seq as u8],
            ))
            .unwrap()
    };
    let first = wrap(1);
    let second = wrap(2);
    let third = wrap(3);
    let limits = CapacityLimits::new(2, 0, 128 * 1024, 0).unwrap();
    let open = || {
        Spool::open(
            &file,
            "test password",
            scope,
            limits,
            NonZeroU64::new(10).unwrap(),
        )
        .unwrap()
    };
    let old;
    {
        let mut spool = open();
        assert_eq!(spool.deposit(first.clone(), 10).unwrap(), Deposit::Stored);
        assert_eq!(
            spool.deposit(first.clone(), 11).unwrap(),
            Deposit::AlreadyStored
        );
        assert_eq!(spool.deposit(second.clone(), 11).unwrap(), Deposit::Stored);
        assert_eq!(
            spool.deposit(third.clone(), 12).unwrap(),
            Deposit::CapacityUnavailable
        );
        assert!(!spool.durable_have(&third.id, 12));
        let page = spool.view(12).delta(&scope, None, 1).unwrap();
        assert_eq!(page.ids, [first.id]);
        assert!(!page.complete);
        assert!(
            spool
                .view(12)
                .bodies(&scope, &page.cursor, &[second.id], 65519)
                .is_err()
        );
        old = page.cursor;
    }
    let mut spool = open();
    let next = spool.view(13).delta(&scope, Some(&old), 1).unwrap();
    assert!(next.complete && !next.reset);
    assert_eq!(next.ids, [second.id]);
    let request = make_get_request(scope, &old, &[first.id]).unwrap();
    let reply = dispatch(&spool.view(13), Some(scope), &request).unwrap();
    let Frame::Events { events, .. } = decode_frame(&reply.bytes).unwrap() else {
        panic!("expected envelope body")
    };
    assert_eq!(events.as_slice(), std::slice::from_ref(&first));

    assert!(!spool.durable_have(&first.id, 20));
    assert!(matches!(
        spool.view(20).bodies(&scope, &old, &[first.id], 65519),
        Err(SpoolError::StaleCursor)
    ));
    assert!(spool.expire_at(20).unwrap());
    assert_eq!(spool.deposit(third.clone(), 20).unwrap(), Deposit::Stored);
    drop(spool);
    let spool = open();
    let request = encode_frame(&Frame::Delta {
        conversation: scope,
        prior: Some(old),
    })
    .unwrap();
    let reply = dispatch(&spool.view(21), Some(scope), &request).unwrap();
    let Frame::Page(page) = decode_frame(&reply.bytes).unwrap() else {
        panic!("expected reset page")
    };
    assert!(page.reset && page.complete);
    assert_eq!(page.ids, [third.id]);
    assert!(!spool.durable_have(&first.id, 21));
    assert!(spool.durable_have(&third.id, 21));
}

#[test]
fn bad_scope_and_signature_do_not_consume_spool_capacity() {
    let path = tempfile::tempdir().unwrap();
    let source = DeviceIdentity::generate();
    let reader = DeviceIdentity::generate();
    let mailbox = RelayMailbox::federated(&source, &reader.public()).unwrap();
    let scope = mailbox.id();
    let mut spool = Spool::open(
        &path.path().join("spool"),
        "pw",
        scope,
        CapacityLimits::new(1, 0, 65519, 0).unwrap(),
        NonZeroU64::new(50).unwrap(),
    )
    .unwrap();
    let inner = Event::new(
        &source,
        ConversationId::new([3; 32]),
        1,
        vec![],
        1,
        0,
        EventKind::Message,
        vec![1],
    );
    let event = mailbox.wrap(&inner).unwrap();
    let mut corrupt = event.clone();
    corrupt.sig[0] ^= 1;
    assert!(matches!(
        spool.deposit(inner, 1),
        Err(SpoolError::InvalidRecord)
    ));
    assert!(matches!(
        spool.deposit(corrupt, 1),
        Err(SpoolError::InvalidRecord)
    ));
    assert_eq!(spool.deposit(event, 2).unwrap(), Deposit::Stored);
}

#[test]
fn spool_rejects_second_writer_and_releases_lease_on_drop() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mailbox");
    let open = || {
        Spool::open(
            &path,
            "test password",
            ConversationId::new([3; 32]),
            CapacityLimits::new(2, 0, 128 * 1024, 0).unwrap(),
            NonZeroU64::new(10).unwrap(),
        )
    };

    let first = open().unwrap();
    assert!(open().is_err(), "two writers opened the same spool");
    drop(first);
    assert!(open().is_ok());
}
