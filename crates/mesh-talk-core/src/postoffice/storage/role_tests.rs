use super::role::{IndexedRelayState, RelayAdmission};
use super::*;
use crate::{
    eventlog::{AppendOutcome, ConversationId, Event, EventKind},
    identity::device::DeviceIdentity,
    storage::profile::{ProfileStorageLease, context::ProfileStorage},
};
use std::sync::Arc;
thread_local! {pub(super) static CUT: std::cell::Cell<Option<&'static str>> = const {std::cell::Cell::new(None)};}

fn open(root: &tempfile::TempDir, identity: &DeviceIdentity) -> Arc<ProfileStorage> {
    ProfileStorage::open_relay(
        ProfileStorageLease::acquire(root.path()).unwrap(),
        &identity.public(),
        &root.path().join("relay.log"),
        "pw",
        10,
    )
    .unwrap()
}
fn event(identity: &DeviceIdentity, seq: u64, parents: Vec<crate::eventlog::EventId>) -> Event {
    Event::new(
        identity,
        ConversationId::new([1; 32]),
        seq,
        parents,
        seq,
        seq,
        EventKind::Message,
        vec![42; 128],
    )
}
#[test]
fn event_acceptance_publishes_arrival_and_charge_together() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = open(&root, &identity);
    let e = event(&identity, 1, vec![]);
    let state =
        IndexedRelayState::from_profile(profile.clone(), RelayAdmission::LegacyCompatible).unwrap();
    assert_eq!(state.record(&e, 10, None).unwrap(), AppendOutcome::Appended);
    let arrivals: i64 = profile
        .database()
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM relay_mailboxes WHERE domain=?1 AND conversation=?2",
                params![
                    RELAY_ARRIVALS.as_slice(),
                    e.conversation_id.as_bytes().as_slice()
                ],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(
        arrivals, 1,
        "accepted relay event must have an atomically published arrival"
    );
}

fn relay(profile: Arc<ProfileStorage>) -> IndexedRelayState {
    IndexedRelayState::from_profile(profile, RelayAdmission::LegacyCompatible).unwrap()
}
fn fed_event(identity: &DeviceIdentity, seq: u64, parents: Vec<crate::eventlog::EventId>) -> Event {
    let mut conv = [1; 32];
    conv[..4].copy_from_slice(b"MTF3");
    Event::new(
        identity,
        ConversationId::new(conv),
        seq,
        parents,
        seq,
        seq,
        EventKind::Message,
        vec![42; 128],
    )
}
#[test]
fn validated_duplicates_preserve_charge_touch_and_can_lower_origin() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = open(&root, &identity);
    let state = relay(profile.clone());
    let e = fed_event(&identity, 1, vec![]);
    let changes = state.subscribe_changes();
    state.record(&e, 100, None).unwrap();
    let before = state.mailbox_metadata(&e.conversation_id).unwrap().unwrap();
    let charge = state.physical_charge().unwrap();
    let generation = *changes.borrow();
    assert_eq!(
        state.record(&e, 110, None).unwrap(),
        AppendOutcome::Duplicate
    );
    assert_eq!(*changes.borrow(), generation);
    let mut forged = e.clone();
    forged.sig[0] ^= 1;
    assert!(matches!(
        state.record(&forged, 110, Some(90)),
        Err(LogError::BadSignature)
    ));
    assert_eq!(
        state.mailbox_metadata(&e.conversation_id).unwrap(),
        Some(before)
    );
    assert_eq!(
        state.record(&e, 110, Some(90)).unwrap(),
        AppendOutcome::Duplicate
    );
    let after = state.mailbox_metadata(&e.conversation_id).unwrap().unwrap();
    assert_eq!(after.first_seen, 90);
    assert_eq!(after.touched, before.touched);
    assert_eq!(after.retained, before.retained);
    assert_eq!(state.physical_charge().unwrap(), charge);
    assert_eq!(*changes.borrow(), generation + 1);
    assert_eq!(state.retained_event(&e.id).unwrap(), Some(e));
}
#[test]
fn missing_cross_conversation_parents_and_equivocation_leave_no_arrival() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = open(&root, &identity);
    let state = relay(profile);
    let a = event(&identity, 1, vec![]);
    state.record(&a, 100, None).unwrap();
    let missing = event(&identity, 2, vec![crate::eventlog::EventId::new([99; 32])]);
    assert!(matches!(
        state.record(&missing, 100, None),
        Err(LogError::MissingParents(_))
    ));
    let cross = fed_event(&identity, 1, vec![a.id]);
    assert!(matches!(
        state.record(&cross, 100, None),
        Err(LogError::MissingParents(_))
    ));
    assert!(
        state
            .mailbox_metadata(&cross.conversation_id)
            .unwrap()
            .is_none()
    );
    let equiv = Event::new(
        &identity,
        a.conversation_id,
        1,
        vec![],
        2,
        2,
        EventKind::Message,
        vec![],
    );
    assert!(matches!(
        state.record(&equiv, 100, None),
        Err(LogError::AuthorEquivocation { .. })
    ));
    assert_eq!(state.physical_charge().unwrap().event_count, 1);
    let child = event(&identity, 2, vec![a.id]);
    state.record(&child, 100, None).unwrap();
    assert_eq!(state.physical_charge().unwrap().estimated_bytes, 384 + 416);
    assert_eq!(state.physical_charge().unwrap().event_count, 2);
    assert_eq!(
        state
            .mailbox_metadata(&a.conversation_id)
            .unwrap()
            .unwrap()
            .touched,
        2
    );
}
#[test]
fn origin_deadline_future_and_unsigned_limits_are_checked() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let state = relay(open(&root, &identity));
    let e = fed_event(&identity, 1, vec![]);
    let age = super::super::MAX_RELAY_AGE.as_secs();
    for (now, origin) in [(100, 401), (age + 100, 100)] {
        assert!(matches!(
            state.record(&e, now, Some(origin)),
            Err(LogError::AdmissionRejected(_))
        ));
        assert_eq!(state.physical_charge().unwrap().event_count, 0);
        assert!(
            state
                .mailbox_metadata(&e.conversation_id)
                .unwrap()
                .is_none()
        );
    }
    state.record(&e, 100, Some(400)).unwrap();
    assert_eq!(
        state
            .mailbox_metadata(&e.conversation_id)
            .unwrap()
            .unwrap()
            .first_seen,
        100
    );
    let root2 = tempfile::tempdir().unwrap();
    let state2 = relay(open(&root2, &identity));
    state2.record(&e, u64::MAX, Some(u64::MAX)).unwrap();
    let m = state2
        .mailbox_metadata(&e.conversation_id)
        .unwrap()
        .unwrap();
    assert_eq!(m.first_seen, u64::MAX);
    assert_eq!(m.expiry_deadline(), None);
}
#[test]
fn runtime_admission_blocks_clear_headers_but_imported_generic_data_stays_readable() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = open(&root, &identity);
    let state =
        IndexedRelayState::from_profile(profile.clone(), RelayAdmission::OpaqueOnly).unwrap();
    let e = event(&identity, 1, vec![]);
    assert!(matches!(
        state.record(&e, 100, None),
        Err(LogError::AdmissionRejected(_))
    ));
    assert_eq!(state.physical_charge().unwrap().event_count, 0);
    let generic =
        IndexedRelayState::from_profile(profile, RelayAdmission::LegacyCompatible).unwrap();
    generic.record(&e, 100, None).unwrap();
    assert_eq!(state.retained_event(&e.id).unwrap(), Some(e));
}
#[test]
fn mutation_and_real_commit_faults_roll_back_and_never_notify() {
    for point in ["event", "arrival", "touch", "runtime", "commit"] {
        let root = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let profile = open(&root, &identity);
        let state = relay(profile.clone());
        let changes = state.subscribe_changes();
        let e = event(&identity, 1, vec![]);
        CUT.with(|v| v.set(Some(point)));
        assert!(state.record(&e, 100, None).is_err(), "{point}");
        CUT.with(|v| v.set(None));
        assert_eq!(*changes.borrow(), 0, "{point}");
        if point == "commit" {
            assert!(state.retained_event(&e.id).is_err());
            assert!(state.physical_charge().is_err());
        } else {
            assert!(state.retained_event(&e.id).unwrap().is_none());
            assert_eq!(state.physical_charge().unwrap().event_count, 0);
        }
        drop(state);
        drop(profile);
        let reopened_profile = open(&root, &identity);
        assert_eq!(reopened_profile.database().read(require_ready).unwrap(), 1);
        let reopened = relay(reopened_profile);
        assert!(reopened.retained_event(&e.id).unwrap().is_none(), "{point}");
        assert!(
            reopened
                .mailbox_metadata(&e.conversation_id)
                .unwrap()
                .is_none()
        );
        assert_eq!(reopened.physical_charge().unwrap().event_count, 0);
        assert_eq!(reopened.physical_charge().unwrap().estimated_bytes, 0);
    }
}
#[test]
fn checked_counter_and_touch_exhaustion_roll_back_the_event() {
    for sql in [
        "UPDATE event_domains SET event_count=9223372036854775807",
        "UPDATE event_domains SET estimated_bytes=9223372036854775807",
        "UPDATE relay_runtime SET next_touch=x'FFFFFFFFFFFFFFFF'",
    ] {
        let root = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let profile = open(&root, &identity);
        let state = relay(profile.clone());
        let e = event(&identity, 1, vec![]);
        profile
            .database()
            .transaction(|c| {
                c.execute_batch(sql)?;
                Ok(())
            })
            .unwrap();
        let before = state.physical_charge().unwrap();
        let changes = state.subscribe_changes();
        assert!(matches!(
            state.record(&e, 100, None),
            Err(LogError::RepositoryCounterExhausted)
        ));
        assert!(state.retained_event(&e.id).unwrap().is_none());
        assert!(
            state
                .mailbox_metadata(&e.conversation_id)
                .unwrap()
                .is_none()
        );
        assert_eq!(state.physical_charge().unwrap(), before);
        assert_eq!(*changes.borrow(), 0);
    }
}
#[test]
fn single_outer_transaction_finishes_and_clones_retain_one_profile_lease() {
    let (send, recv) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let root = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let profile = open(&root, &identity);
        let state = Arc::new(relay(profile.clone()));
        let clone = state.clone();
        let e = event(&identity, 1, vec![]);
        drop(profile);
        state.record(&e, 100, None).unwrap();
        assert!(ProfileStorageLease::acquire(root.path()).is_err());
        drop(state);
        assert_eq!(clone.retained_event(&e.id).unwrap(), Some(e));
        assert!(ProfileStorageLease::acquire(root.path()).is_err());
        drop(clone);
        drop(ProfileStorageLease::acquire(root.path()).unwrap());
        send.send(()).unwrap();
    });
    recv.recv_timeout(std::time::Duration::from_secs(10))
        .expect("relay transaction nested the shared database mutex");
    worker.join().unwrap();
}

#[test]
fn opaque_wire_event_commits_and_reopens_with_the_same_physical_metadata() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let recipient = DeviceIdentity::generate();
    let office = DeviceIdentity::generate();
    let mailbox = crate::postoffice::opaque::RelayMailbox::new(
        &identity,
        &recipient.public(),
        &office.public(),
    )
    .unwrap();
    let e = mailbox.wrap(&event(&identity, 1, vec![])).unwrap();
    let profile = open(&root, &identity);
    let state =
        IndexedRelayState::from_profile(profile.clone(), RelayAdmission::OpaqueOnly).unwrap();
    state.record(&e, 100, None).unwrap();
    let charge = state.physical_charge().unwrap();
    let metadata = state.mailbox_metadata(&e.conversation_id).unwrap();
    assert_eq!(charge.event_count, 1);
    assert_eq!(charge.estimated_bytes, e.ciphertext.len() as u64 + 256);
    drop(state);
    drop(profile);
    let state = IndexedRelayState::from_profile(open(&root, &identity), RelayAdmission::OpaqueOnly)
        .unwrap();
    assert_eq!(state.physical_charge().unwrap(), charge);
    assert_eq!(
        state.mailbox_metadata(&e.conversation_id).unwrap(),
        metadata
    );
    assert_eq!(state.retained_event(&e.id).unwrap(), Some(e));
}
#[test]
fn warm_reopen_uses_only_fixed_authority_and_never_hydrates_history() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let path = root.path().join("relay.log");
    let (mut legacy, _) = EncryptedRecordLog::<Event>::open(&path, "pw", b"MTLOG1").unwrap();
    for i in 0..128u8 {
        let e = Event::new(
            &identity,
            ConversationId::new([i; 32]),
            1,
            vec![],
            1,
            1,
            EventKind::Message,
            vec![7; 4096],
        );
        legacy.append_durable(&e).unwrap();
    }
    drop(legacy);
    let profile = open(&root, &identity);
    assert_eq!(
        relay(profile.clone())
            .physical_charge()
            .unwrap()
            .event_count,
        128
    );
    profile
        .database()
        .transaction(|c| {
            // Corrupt history bytes are detected by a selected exact read, not an
            // implicit full-history read while opening the internal role owner.
            c.execute("UPDATE retained_events SET body=zeroblob(length(body))", [])?;
            Ok(())
        })
        .unwrap();
    drop(profile);
    crate::storage::profile::publication::tests::HASHES.with(|v| v.set(0));
    crate::storage::profile::publication::tests::DECODES.with(|v| v.set(0));
    crate::storage::database::observations::CONNECTION_OPENS.with(|v| v.set(0));
    let profile = open(&root, &identity);
    assert_eq!(
        crate::storage::database::observations::CONNECTION_OPENS.with(|v| v.get()),
        1
    );
    let state = relay(profile.clone());
    let additional = IndexedRelayState::from_profile(profile, RelayAdmission::OpaqueOnly).unwrap();
    assert_eq!(additional.physical_charge().unwrap().event_count, 128);
    assert_eq!(
        crate::storage::database::observations::CONNECTION_OPENS.with(|v| v.get()),
        1,
        "role facades must share the existing keyed connection and its cache"
    );
    assert_eq!(state.physical_charge().unwrap().event_count, 128);
    assert_eq!(
        crate::storage::profile::publication::tests::HASHES.with(|v| v.get()),
        0
    );
    assert_eq!(
        crate::storage::profile::publication::tests::DECODES.with(|v| v.get()),
        0
    );
}

#[test]
fn duplicate_origin_correction_failure_rolls_back_and_read_errors_propagate() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = open(&root, &identity);
    let state = relay(profile.clone());
    let changes = state.subscribe_changes();
    let e = fed_event(&identity, 1, vec![]);
    state.record(&e, 100, None).unwrap();
    let before = state.mailbox_metadata(&e.conversation_id).unwrap();
    let generation = *changes.borrow();
    CUT.with(|v| v.set(Some("runtime")));
    assert!(state.record(&e, 110, Some(90)).is_err());
    CUT.with(|v| v.set(None));
    assert_eq!(state.mailbox_metadata(&e.conversation_id).unwrap(), before);
    assert_eq!(*changes.borrow(), generation);
    profile
        .database()
        .transaction(|c| {
            c.execute_batch("DROP TABLE relay_mailboxes")?;
            Ok(())
        })
        .unwrap();
    assert!(state.retained_event(&e.id).is_err());
    assert!(state.mailbox_metadata(&e.conversation_id).is_err());
    assert!(state.record(&e, 110, None).is_err());
    assert_eq!(*changes.borrow(), generation);
}

#[test]
fn retirement_keeps_honest_physical_mailbox_metadata_and_token_holds_profile_lease() {
    use crate::eventlog::repository::retirement::{RetirementBegin, RetirementBudget};
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = open(&root, &identity);
    let state = relay(profile.clone());
    let e = event(&identity, 1, vec![]);
    state.record(&e, 100, None).unwrap();
    let repo = profile.events().unwrap();
    let token = match profile
        .database()
        .transaction(|c| repo.begin_retirement_on(c, &e.conversation_id))
        .unwrap()
    {
        RetirementBegin::Started(t) => t,
        _ => panic!(),
    };
    let m = state.mailbox_metadata(&e.conversation_id).unwrap().unwrap();
    assert!(m.retained.retiring);
    assert_eq!(m.retained.count, 1);
    assert_eq!(state.physical_charge().unwrap().event_count, 1);
    assert!(matches!(
        state.retained_event(&e.id),
        Err(LogError::ConversationRetiring)
    ));
    assert!(matches!(
        state.record(&e, 110, None),
        Err(LogError::ConversationRetiring)
    ));
    let b = RetirementBudget {
        auxiliary: 1,
        edges: 1,
        vertices: 1,
        bytes: 256,
    };
    for _ in 0..100 {
        let p = profile
            .database()
            .transaction(|c| repo.reclaim_retirement_on(c, &token, b))
            .unwrap();
        assert!(p.transaction_latency.is_none());
        if p.phase == 7 && !p.complete {
            let m = state.mailbox_metadata(&e.conversation_id).unwrap().unwrap();
            assert!(m.retained.retiring);
            assert_eq!(m.retained.count, 0);
            assert_eq!(state.physical_charge().unwrap().event_count, 0);
            break;
        }
        assert!(!p.complete);
    }
    drop(state);
    drop(repo);
    drop(profile);
    assert!(ProfileStorageLease::acquire(root.path()).is_err());
    drop(token);
    drop(ProfileStorageLease::acquire(root.path()).unwrap());
}
