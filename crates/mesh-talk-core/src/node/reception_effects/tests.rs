use super::*;
use crate::identity::{account::Account, device::DeviceIdentity};
use crate::node::received_store::ReceivedStore;
use crate::storage::profile::ProfileStorageLease;

struct Fixture {
    effects: ReceptionEffects,
    received: ReceivedStore,
    identity: DeviceIdentity,
    account: Account,
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let profile = ProfileStorage::open_participant(
            ProfileStorageLease::acquire(dir.path()).unwrap(),
            &identity.public(),
            &account.public(),
            &dir.path().join("messages.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        profile.complete_participant("pw", None).unwrap();
        drop(
            crate::eventlog::persist::PersistentEventLog::open_profile(profile.clone(), "pw")
                .unwrap(),
        );
        let received =
            ReceivedStore::from_profile(profile.clone(), &dir.path().join("received.log"), "pw")
                .unwrap();
        let effects = ReceptionEffects::open(profile).unwrap();
        Self {
            _dir: dir,
            effects,
            received,
            identity,
            account,
        }
    }
    fn reopen(self) -> Self {
        let Self {
            _dir,
            effects,
            received,
            identity,
            account,
        } = self;
        drop(received);
        drop(effects);
        let profile = ProfileStorage::open_participant(
            ProfileStorageLease::acquire(_dir.path()).unwrap(),
            &identity.public(),
            &account.public(),
            &_dir.path().join("messages.log"),
            &_dir.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        profile.complete_participant("pw", None).unwrap();
        drop(
            crate::eventlog::persist::PersistentEventLog::open_profile(profile.clone(), "pw")
                .unwrap(),
        );
        let received =
            ReceivedStore::from_profile(profile.clone(), &_dir.path().join("received.log"), "pw")
                .unwrap();
        let effects = ReceptionEffects::open(profile).unwrap();
        Self {
            _dir,
            effects,
            received,
            identity,
            account,
        }
    }
    fn insert(&mut self, n: u8, bytes: usize) -> (ReceptionSource, u64) {
        let entry = ReceivedEntry {
            event_id: EventId::new([n; 32]),
            conversation: ConversationId::new([1; 32]),
            from: "peer-account".into(),
            wall_clock: u64::from(255 - n),
            plaintext: vec![b'x'; bytes],
        };
        self.received.record_durable(&entry).unwrap();
        let source = ReceptionSource {
            kind: ReceptionKind::DirectMessage,
            conversation: entry.conversation,
            event_id: entry.event_id,
        };
        let sequence = self
            .effects
            .profile
            .database()
            .transaction(|c| install_on(c, source, &metadata()))
            .unwrap();
        (source, sequence)
    }
    fn zero(&self) -> ReceptionCursor {
        ReceptionCursor {
            sequence: 0,
            ..self.effects.head().unwrap()
        }
    }
}

fn metadata() -> ReceptionMetadata {
    ReceptionMetadata {
        from_device: "peer-device".into(),
        received_at: 100,
        arrival_kind: ArrivalKind::Live,
        own_device: false,
        batch_id: None,
        batch_complete: true,
        batch_size: 1,
        reception_only: false,
    }
}

#[test]
fn account_adoption_fences_old_handles_and_keeps_each_accounts_pending_work() {
    use crate::storage::profile::participant::{finish_adoption, prepare_adoption};
    let mut f = Fixture::new();
    let zero = f.zero();
    let (source, original_sequence) = f.insert(1, 20);
    let original_head = f.effects.head().unwrap();
    f.effects
        .register(ReceptionConsumer::Desktop, zero)
        .unwrap();
    let next_account = Account::generate();
    let database = f.effects.profile.database();
    prepare_adoption(database, &f.account.public(), &next_account.public()).unwrap();
    finish_adoption(database, &next_account.public()).unwrap();
    assert!(matches!(f.effects.head(), Err(LogError::StaleCursor)));
    assert!(matches!(
        f.effects.page(zero, original_head, limit(10, 1000), 100),
        Err(LogError::StaleCursor)
    ));
    assert!(matches!(
        f.effects.register(ReceptionConsumer::Desktop, zero),
        Err(LogError::StaleCursor)
    ));
    assert!(matches!(
        f.effects
            .acknowledge(ReceptionConsumer::Desktop, zero, original_head),
        Err(LogError::StaleCursor)
    ));
    assert!(matches!(f.effects.retire(10), Err(LogError::StaleCursor)));

    // The producer derives authority inside the body transaction, even before
    // a new runtime has opened a reader for the adopted account.
    let installed = database
        .transaction(|c| install_on(c, source, &metadata()))
        .unwrap();
    assert_ne!(installed, original_sequence);
    let next = ReceptionEffects::open(f.effects.profile.clone()).unwrap();
    let next_head = next.head().unwrap();
    assert_ne!(next_head.epoch, original_head.epoch);
    let next_zero = ReceptionCursor {
        sequence: 0,
        ..next_head
    };
    let page = next
        .page(next_zero, next_head, limit(10, 1000), 100)
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].sequence, installed);
    next.register(ReceptionConsumer::Desktop, next_zero)
        .unwrap();
    next.acknowledge(ReceptionConsumer::Desktop, next_zero, next_head)
        .unwrap();
    assert_eq!(next.retire(10).unwrap(), 1);

    prepare_adoption(database, &next_account.public(), &f.account.public()).unwrap();
    finish_adoption(database, &f.account.public()).unwrap();
    assert!(matches!(f.effects.head(), Err(LogError::StaleCursor)));
    assert!(matches!(f.effects.oldest(), Err(LogError::StaleCursor)));
    assert!(matches!(
        f.effects.registered(ReceptionConsumer::Desktop),
        Err(LogError::StaleCursor)
    ));
    assert!(matches!(
        f.effects.page(zero, original_head, limit(10, 1000), 100),
        Err(LogError::StaleCursor)
    ));
    assert!(matches!(
        f.effects.register(ReceptionConsumer::Desktop, zero),
        Err(LogError::StaleCursor)
    ));
    assert!(matches!(
        f.effects
            .acknowledge(ReceptionConsumer::Desktop, zero, original_head),
        Err(LogError::StaleCursor)
    ));
    assert!(matches!(f.effects.retire(10), Err(LogError::StaleCursor)));
    let restored = ReceptionEffects::open(f.effects.profile.clone()).unwrap();
    assert_eq!(restored.head().unwrap(), original_head);
    let restored_page = restored
        .page(zero, original_head, limit(10, 1000), 100)
        .unwrap();
    assert_eq!(restored_page.rows.len(), 1);
    assert_eq!(restored_page.rows[0].sequence, original_sequence);
    assert_eq!(
        restored
            .register(ReceptionConsumer::Desktop, original_head)
            .unwrap(),
        zero
    );
    assert!(matches!(next.head(), Err(LogError::StaleCursor)));
}
fn limit(rows: usize, bytes: usize) -> ReceptionLimit {
    ReceptionLimit {
        rows,
        body_bytes: bytes,
        serialized_bytes: 64 * 1024 * 1024,
    }
}

#[test]
fn installation_order_and_original_metadata_survive_duplicate_install() {
    let mut f = Fixture::new();
    let zero = f.zero();
    let (source, first) = f.insert(1, 20);
    f.insert(2, 20);
    let mut changed = metadata();
    changed.received_at = 999;
    let duplicate = f
        .effects
        .profile
        .database()
        .transaction(|c| install_on(c, source, &changed))
        .unwrap();
    assert_eq!(first, duplicate);
    let page = f
        .effects
        .page(zero, f.effects.head().unwrap(), limit(10, 1000), 100)
        .unwrap();
    assert_eq!(page.rows.len(), 2);
    assert_eq!(page.rows[0].metadata.received_at, 100);
    assert_eq!(page.rows[0].source, source);
    assert!(
        page.rows[0].entry.as_ref().unwrap().wall_clock
            > page.rows[1].entry.as_ref().unwrap().wall_clock
    );
    assert!(page.complete);
}

#[test]
fn row_and_raw_byte_limits_bound_a_stalled_consumers_demand_page() {
    let mut f = Fixture::new();
    let zero = f.zero();
    for n in 1..40 {
        f.insert(n, 1024);
    }
    let through = f.effects.head().unwrap();
    let first = f
        .effects
        .page(zero, through, limit(3, 10_000), 100)
        .unwrap();
    assert_eq!(first.rows.len(), 3);
    assert!(!first.complete);
    let second = f
        .effects
        .page(first.next, through, limit(20, 2200), 100)
        .unwrap();
    assert_eq!(second.rows.len(), 2);
    assert_eq!(second.body_bytes, 2072);
    assert!(
        second
            .rows
            .iter()
            .all(|row| row.sequence > first.next.sequence)
    );
    assert!(second.serialized_bound <= limit(20, 2200).serialized_bytes);
}

#[test]
fn serialized_byte_admission_precedes_body_materialization() {
    let mut f = Fixture::new();
    let zero = f.zero();
    f.insert(1, 1024);
    let small = ReceptionLimit {
        rows: 1,
        body_bytes: 4096,
        serialized_bytes: 100,
    };
    assert!(
        f.effects
            .page(zero, f.effects.head().unwrap(), small, 100)
            .is_err()
    );
    assert_eq!(
        f.effects
            .body_reads
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    let page = f
        .effects
        .page(zero, f.effects.head().unwrap(), limit(1, 4096), 100)
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert!(page.serialized_bound >= 6 * 1024);
}

#[test]
fn deleting_source_advances_cursor_and_keeps_terminal_batch_effect() {
    let mut f = Fixture::new();
    let zero = f.zero();
    let (source, _) = f.insert(1, 30);
    f.insert(2, 30);
    let terminal_source = ReceptionSource {
        kind: ReceptionKind::ChannelTerminal,
        ..source
    };
    let mut terminal = metadata();
    terminal.reception_only = true;
    terminal.batch_id = Some("batch-1".into());
    terminal.arrival_kind = ArrivalKind::CatchUp;
    f.effects
        .profile
        .database()
        .transaction(|c| install_on(c, terminal_source, &terminal))
        .unwrap();
    f.received
        .remove_where(|row| row.event_id == source.event_id)
        .unwrap();
    let page = f
        .effects
        .page(zero, f.effects.head().unwrap(), limit(10, 1000), 100)
        .unwrap();
    assert_eq!(page.rows.len(), 2);
    assert_eq!(page.rows[0].source.event_id, EventId::new([2; 32]));
    assert!(page.rows[1].entry.is_none());
    assert!(page.rows[1].metadata.reception_only);
    assert_eq!(page.next, page.through);
    assert!(page.complete);
}

#[test]
fn frozen_through_cursor_excludes_concurrent_later_installations() {
    let mut f = Fixture::new();
    let zero = f.zero();
    f.insert(1, 30);
    let through = f.effects.head().unwrap();
    f.insert(2, 30);
    let page = f.effects.page(zero, through, limit(10, 1000), 100).unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.next, through);
    assert!(page.complete);
    let later = f
        .effects
        .page(page.next, f.effects.head().unwrap(), limit(10, 1000), 100)
        .unwrap();
    assert_eq!(later.rows.len(), 1);
}

#[test]
fn failed_transaction_cannot_publish_receipt_or_advance_head() {
    let mut f = Fixture::new();
    let (source, _) = f.insert(1, 30);
    let before = f.effects.head().unwrap();
    let terminal_source = ReceptionSource {
        kind: ReceptionKind::ChannelTerminal,
        ..source
    };
    let mut terminal = metadata();
    terminal.reception_only = true;
    terminal.batch_id = Some("batch-1".into());
    let result: Result<(), LogError> = f.effects.profile.database().transaction(|c| {
        install_on(c, terminal_source, &terminal)?;
        Err(LogError::CorruptFile("injected completion failure".into()))
    });
    assert!(result.is_err());
    assert_eq!(f.effects.head().unwrap(), before);
}

#[test]
fn retirement_waits_for_every_fixed_consumer_and_preserves_installation_identity() {
    let mut f = Fixture::new();
    let zero = f.zero();
    let (source, first) = f.insert(1, 30);
    f.insert(2, 30);
    let head = f.effects.head().unwrap();
    f.effects
        .register(ReceptionConsumer::CliOutput, zero)
        .unwrap();
    f.effects
        .register(ReceptionConsumer::CliAutosave, zero)
        .unwrap();
    f.effects
        .acknowledge(ReceptionConsumer::CliOutput, zero, head)
        .unwrap();
    assert_eq!(f.effects.retire(10).unwrap(), 0);
    f.effects
        .acknowledge(ReceptionConsumer::CliAutosave, zero, head)
        .unwrap();
    assert_eq!(f.effects.retire(1).unwrap(), 1);
    assert_eq!(f.effects.retire(1).unwrap(), 1);
    let duplicate = f
        .effects
        .profile
        .database()
        .transaction(|c| install_on(c, source, &metadata()))
        .unwrap();
    assert_eq!(duplicate, first);
    assert_eq!(f.effects.head().unwrap(), head);
    assert!(f.effects.page(zero, head, limit(10, 1000), 100).is_err());
    let page = f.effects.page(head, head, limit(10, 1000), 100).unwrap();
    assert!(page.rows.is_empty());
}

#[test]
fn stale_epoch_and_stale_progress_acknowledgements_fail_without_advancing_consumer() {
    let mut f = Fixture::new();
    let zero = f.zero();
    f.insert(1, 30);
    let head = f.effects.head().unwrap();
    f.effects
        .register(ReceptionConsumer::Desktop, zero)
        .unwrap();
    let stale = ReceptionCursor {
        epoch: [9; 16],
        ..zero
    };
    assert!(f.effects.page(stale, head, limit(10, 1000), 100).is_err());
    assert!(
        f.effects
            .acknowledge(ReceptionConsumer::Desktop, head, head)
            .is_err()
    );
    assert_eq!(
        f.effects
            .register(ReceptionConsumer::Desktop, head)
            .unwrap(),
        zero
    );
    f.effects
        .acknowledge(ReceptionConsumer::Desktop, zero, head)
        .unwrap();
}

#[test]
fn reopen_retains_epoch_pending_prefix_acknowledgement_and_first_reception_time() {
    let mut f = Fixture::new();
    let zero = f.zero();
    f.insert(1, 30);
    let first = f.effects.head().unwrap();
    f.insert(2, 30);
    let head = f.effects.head().unwrap();
    f.effects
        .register(ReceptionConsumer::Desktop, zero)
        .unwrap();
    f.effects
        .acknowledge(ReceptionConsumer::Desktop, zero, first)
        .unwrap();
    let f = f.reopen();
    assert_eq!(f.effects.head().unwrap(), head);
    assert_eq!(
        f.effects
            .register(ReceptionConsumer::Desktop, head)
            .unwrap(),
        first
    );
    let page = f.effects.page(first, head, limit(10, 1000), 100).unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].metadata.received_at, 100);
    assert_eq!(
        f.effects
            .body_reads
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}

#[test]
fn body_and_reception_install_rollback_together_under_existing_transcript_transaction() {
    use crate::node::transcript_repository::{RECEIVED, TranscriptRepository};
    let f = Fixture::new();
    let before = f.effects.head().unwrap();
    let source = ReceptionSource {
        kind: ReceptionKind::DirectMessage,
        conversation: ConversationId::new([1; 32]),
        event_id: EventId::new([1; 32]),
    };
    let result: Result<(), LogError> = f.effects.profile.database().transaction(|c| {
        TranscriptRepository::put_on(
            c,
            RECEIVED,
            source.conversation,
            source.event_id.as_bytes(),
            10,
            0,
            "peer",
            b"body",
            false,
            false,
        )?;
        install_on(c, source, &metadata())?;
        Err(LogError::CorruptFile(
            "failure before accepted transaction completes".into(),
        ))
    });
    assert!(result.is_err());
    assert!(!f.received.contains(source.event_id).unwrap());
    assert_eq!(f.effects.head().unwrap(), before);
    assert!(
        f.effects
            .page(before, before, limit(1, 100), 100)
            .unwrap()
            .rows
            .is_empty()
    );
}

#[test]
fn invisible_source_scan_is_bounded_and_exact_suppression_wins_before_body_read() {
    let mut f = Fixture::new();
    let zero = f.zero();
    let (source, _) = f.insert(1, 1024);
    f.insert(2, 1024);
    f.insert(3, 1024);
    f.effects
        .profile
        .database()
        .transaction(|c| {
            c.execute(
                "INSERT INTO transcript_suppression VALUES(?1,?2)",
                rusqlite::params![
                    source.conversation.as_bytes().as_slice(),
                    source.event_id.as_bytes().as_slice()
                ],
            )?;
            c.execute(
                "UPDATE transcript_rows SET deadline=?1 WHERE row_key=?2",
                rusqlite::params![50_u64.to_be_bytes().as_slice(), [2_u8; 32].as_slice()],
            )?;
            Ok(())
        })
        .unwrap();
    let head = f.effects.head().unwrap();
    let hidden = f.effects.page(zero, head, limit(1, 100), 100).unwrap();
    assert!(hidden.rows.is_empty());
    assert!(!hidden.complete);
    assert_eq!(hidden.next.sequence, 1);
    assert_eq!(
        f.effects
            .body_reads
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    let expired = f
        .effects
        .page(hidden.next, head, limit(1, 100), 100)
        .unwrap();
    assert!(expired.rows.is_empty());
    assert_eq!(expired.next.sequence, 2);
    let last = f
        .effects
        .page(expired.next, head, limit(1, 2048), 100)
        .unwrap();
    assert_eq!(last.rows.len(), 1);
    assert!(last.complete);
}

#[test]
fn maximum_transcript_row_fits_explicit_serialized_budget_without_full_backlog_loading() {
    let mut f = Fixture::new();
    let zero = f.zero();
    f.insert(1, MAX_RECEPTION_BODY_BYTES - "peer-account".len());
    f.insert(2, 30);
    let page = f
        .effects
        .page(
            zero,
            f.effects.head().unwrap(),
            limit(128, MAX_RECEPTION_BODY_BYTES),
            100,
        )
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.body_bytes, MAX_RECEPTION_BODY_BYTES);
    assert!(!page.complete);
    assert!(page.serialized_bound <= MAX_RECEPTION_SERIALIZED_BYTES);
    assert_eq!(
        f.effects
            .body_reads
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}
