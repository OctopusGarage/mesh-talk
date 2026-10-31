use super::super::role::maintenance::RelayMaintenanceBudget;
use super::*;

pub(super) fn scoped(identity: &DeviceIdentity, key: u8) -> Event {
    Event::new(
        identity,
        ConversationId::new([key; 32]),
        1,
        vec![],
        1,
        1,
        EventKind::Message,
        vec![1; 128],
    )
}
fn changes(profile: &ProfileStorage) -> u64 {
    profile.database().read(|c| Ok(c.total_changes())).unwrap()
}
fn cutoff(profile: &ProfileStorage) -> Option<u64> {
    profile
        .database()
        .read(|c| {
            let bytes: Option<Vec<u8>> =
                c.query_row("SELECT expired_cutoff FROM relay_runtime", [], |r| r.get(0))?;
            bytes
                .map(|v| Ok(u64::from_be_bytes(v.try_into().unwrap())))
                .transpose()
        })
        .unwrap()
}
#[test]
fn zero_exact_deadline_idle_rollback_reopen_and_current_custody() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = open(&root, &identity);
    let state = relay(profile.clone());
    let e = event(&identity, 1, vec![]);
    state.record(&e, 0, None).unwrap();
    let age = super::super::super::MAX_RELAY_AGE.as_secs();
    let view = state
        .policy_view_at(&e.conversation_id, age - 1)
        .unwrap()
        .unwrap();
    let watch = state.subscribe_changes();
    let before = changes(&profile);
    for _ in 0..5 {
        assert!(
            state
                .durable_have_at(&e.conversation_id, &e.id, age - 1)
                .unwrap()
        );
        assert_eq!(state.next_work_at(age - 1).unwrap().next_expiry, Some(age));
    }
    assert_eq!(changes(&profile), before);
    assert_eq!(cutoff(&profile), None);
    let generation = *watch.borrow();
    assert!(matches!(
        state.durable_have_at(&e.conversation_id, &e.id, age),
        Err(LogError::RelayMailboxUnavailable)
    ));
    assert_eq!(cutoff(&profile), Some(0));
    assert_eq!(*watch.borrow(), generation + 1);
    assert!(matches!(
        state.retained_event_at(&e.id, age),
        Err(LogError::RelayMailboxUnavailable)
    ));
    assert!(matches!(
        state.validate_policy_view_at(&view, 0),
        Err(LogError::StaleCursor)
    ));
    let before = changes(&profile);
    assert!(state.mailbox_metadata_at(&e.conversation_id, 0).is_err());
    assert!(
        state
            .mailbox_metadata_at(&e.conversation_id, age + 1)
            .is_err()
    );
    assert_eq!(changes(&profile), before);
    assert_eq!(state.physical_charge().unwrap().event_count, 1);
    drop(view);
    drop(state);
    drop(profile);
    let state = relay(open(&root, &identity));
    assert!(state.durable_have_at(&e.conversation_id, &e.id, 0).is_err());
    assert!(state.record(&scoped(&identity, 2), 0, None).is_err());
}
#[test]
fn max_deadline_never_wraps_and_unknown_id_never_has_custody() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = open(&root, &identity);
    let state = relay(profile.clone());
    let e = event(&identity, 1, vec![]);
    state.record(&e, u64::MAX, None).unwrap();
    assert_eq!(state.next_work_at(u64::MAX).unwrap().next_expiry, None);
    assert_eq!(cutoff(&profile), None);
    assert!(
        state
            .durable_have_at(&e.conversation_id, &e.id, u64::MAX)
            .unwrap()
    );
    assert!(
        !state
            .durable_have_at(
                &e.conversation_id,
                &crate::eventlog::EventId::new([99; 32]),
                u64::MAX
            )
            .unwrap()
    );
    assert!(
        !state
            .durable_have_at(&ConversationId::new([3; 32]), &e.id, u64::MAX)
            .unwrap()
    );
}
#[test]
fn valid_earlier_origin_after_clock_rollback_commits_hidden_but_expired_hint_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = open(&root, &identity);
    let state = relay(profile.clone());
    let a = fed_event(&identity, 1, vec![]);
    let b = scoped(&identity, 2);
    state.record(&a, 1000, None).unwrap();
    state.record(&b, 100, None).unwrap();
    let age = super::super::super::MAX_RELAY_AGE.as_secs();
    state.next_work_at(age + 200).unwrap();
    assert_eq!(cutoff(&profile), Some(200));
    assert!(matches!(
        state.record(&a, age + 200, Some(100)),
        Err(LogError::AdmissionRejected(_))
    ));
    let before = state.mailbox_metadata(&a.conversation_id).unwrap().unwrap();
    let mut forged = a.clone();
    forged.sig[0] ^= 1;
    assert!(matches!(
        state.record(&forged, 150, Some(100)),
        Err(LogError::BadSignature)
    ));
    assert_eq!(
        state.mailbox_metadata(&a.conversation_id).unwrap().unwrap(),
        before
    );
    assert!(matches!(
        state.record(&a, 150, Some(100)),
        Err(LogError::RelayMailboxUnavailable)
    ));
    let after = state.mailbox_metadata(&a.conversation_id).unwrap().unwrap();
    assert_eq!(after.first_seen, 100);
    assert_eq!(after.touched, before.touched);
    assert!(state.retained_event_at(&a.id, 150).is_err());
}
#[test]
fn count_envelope_commits_backpressure_only_for_valid_growth_and_preserves_duplicates() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = open(&root, &identity);
    let mut state = relay(profile.clone());
    state.set_limits_for_test(1, 1, 1_000_000, 0).unwrap();
    let a = scoped(&identity, 1);
    let b = scoped(&identity, 2);
    let c = scoped(&identity, 3);
    state.record(&a, 100, None).unwrap();
    state.record(&b, 100, None).unwrap();
    assert!(!state.next_work_at(100).unwrap().drain_requested);
    let mut forged = c.clone();
    forged.sig[0] ^= 1;
    assert!(matches!(
        state.record(&forged, 100, None),
        Err(LogError::BadSignature)
    ));
    assert!(!state.next_work_at(100).unwrap().drain_requested);
    let charge = state.physical_charge().unwrap();
    let before = state.mailbox_metadata(&a.conversation_id).unwrap();
    assert_eq!(
        state.record(&a, 100, None).unwrap(),
        AppendOutcome::Duplicate
    );
    assert_eq!(state.mailbox_metadata(&a.conversation_id).unwrap(), before);
    let error = state.record(&c, 100, None).unwrap_err();
    assert!(matches!(error, LogError::RelayCapacityUnavailable));
    assert!(!error.is_peer_rejection());
    assert_eq!(state.physical_charge().unwrap(), charge);
    assert!(state.retained_event_at(&c.id, 100).unwrap().is_none());
    assert!(
        state
            .mailbox_metadata(&c.conversation_id)
            .unwrap()
            .is_none()
    );
    assert!(state.next_work_at(100).unwrap().drain_requested);
    drop(state);
    drop(profile);
    assert!(
        relay(open(&root, &identity))
            .next_work_at(100)
            .unwrap()
            .drain_requested
    );
}
#[test]
fn byte_equality_impossible_candidate_and_limit_overflow_are_distinct() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let mut state = relay(open(&root, &identity));
    state.set_limits_for_test(100, 0, 384, 384).unwrap();
    state.record(&scoped(&identity, 1), 100, None).unwrap();
    state.record(&scoped(&identity, 2), 100, None).unwrap();
    assert_eq!(state.physical_charge().unwrap().estimated_bytes, 768);
    assert!(matches!(
        state.record(&scoped(&identity, 3), 100, None),
        Err(LogError::RelayCapacityUnavailable)
    ));
    let root = tempfile::tempdir().unwrap();
    let mut state = relay(open(&root, &identity));
    state.set_limits_for_test(10, 0, 383, 0).unwrap();
    let error = state.record(&scoped(&identity, 4), 100, None).unwrap_err();
    assert!(matches!(error, LogError::RelayEventTooLarge));
    assert!(!error.is_peer_rejection());
    assert_eq!(state.physical_charge().unwrap().event_count, 0);
    assert!(!state.next_work_at(100).unwrap().drain_requested);
    assert!(state.set_limits_for_test(u64::MAX, 1, 1, 0).is_err());
    assert!(state.set_limits_for_test(1, 0, u64::MAX, 1).is_err());
}
#[test]
fn pending_retirement_is_serialized_and_drain_resumes_to_both_base_targets() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = open(&root, &identity);
    let mut state = relay(profile.clone());
    state.set_limits_for_test(2, 2, 384, 768).unwrap();
    for key in 1..=3 {
        state.record(&scoped(&identity, key), 100, None).unwrap();
    }
    assert!(matches!(
        state.record(&scoped(&identity, 4), 100, None),
        Err(LogError::RelayCapacityUnavailable)
    ));
    let budget = RelayMaintenanceBudget::tiny();
    let first = state.maintenance_step_at(100, budget).unwrap();
    assert_eq!(first.selected_scopes, 1);
    for _ in 0..3 {
        let p = state.maintenance_step_at(100, budget).unwrap();
        assert_eq!(p.selected_scopes, 0);
        assert!(p.examined <= budget.examined);
        assert_eq!(state.physical_charge().unwrap().event_count, 3);
        assert!(
            state
                .mailbox_metadata_at(&ConversationId::new([2; 32]), 100)
                .unwrap()
                .is_some()
        );
    }
    drop(state);
    drop(profile);
    let profile = open(&root, &identity);
    let mut state = relay(profile.clone());
    state.set_limits_for_test(2, 2, 384, 768).unwrap();
    let mut added_during_drain = false;
    for _ in 0..200 {
        let p = state.maintenance_step_at(100, budget).unwrap();
        assert!(p.examined <= budget.examined);
        assert!(p.selected_scopes <= 1);
        profile
            .database()
            .read(|c| {
                let count: i64 =
                    c.query_row("SELECT count(*) FROM retained_events", [], |r| r.get(0))?;
                assert_eq!(state_physical_on(c)?, count);
                assert!(
                    c.prepare("PRAGMA foreign_key_check")?
                        .query([])?
                        .next()?
                        .is_none()
                );
                Ok(())
            })
            .unwrap();
        if state.physical_charge().unwrap().event_count == 2 && !added_during_drain {
            assert!(
                state.next_work_at(100).unwrap().drain_requested,
                "below-envelope pressure still drains to both bases"
            );
            state.record(&scoped(&identity, 4), 100, None).unwrap();
            assert!(
                state.next_work_at(100).unwrap().drain_requested,
                "new arrivals cannot cancel persisted drain"
            );
            added_during_drain = true;
        }
        if !state.next_work_at(100).unwrap().drain_requested {
            break;
        }
    }
    assert!(added_during_drain);
    assert!(!state.next_work_at(100).unwrap().drain_requested);
    assert_eq!(state.physical_charge().unwrap().event_count, 1);
    assert_eq!(state.physical_charge().unwrap().estimated_bytes, 384);
}
fn state_physical_on(c: &Connection) -> Result<i64, LogError> {
    Ok(c.query_row(
        "SELECT event_count FROM event_domains WHERE domain=?1",
        [RELAY_EVENTS.0.as_slice()],
        |r| r.get(0),
    )?)
}
#[test]
fn cutoff_and_backpressure_commit_failure_never_publish_positive_custody_or_watch() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = open(&root, &identity);
    let state = relay(profile.clone());
    let e = scoped(&identity, 1);
    state.record(&e, 0, None).unwrap();
    let watch = state.subscribe_changes();
    let before = *watch.borrow();
    let age = super::super::super::MAX_RELAY_AGE.as_secs();
    CUT.with(|v| v.set(Some("cutoff")));
    assert!(state.retained_event_at(&e.id, age).is_err());
    CUT.with(|v| v.set(None));
    assert_eq!(cutoff(&profile), None);
    assert_eq!(*watch.borrow(), before);
    CUT.with(|v| v.set(Some("commit")));
    assert!(
        state
            .durable_have_at(&e.conversation_id, &e.id, age)
            .is_err()
    );
    CUT.with(|v| v.set(None));
    assert!(state.physical_charge().is_err());
    assert_eq!(*watch.borrow(), before);
    drop(state);
    drop(profile);
    let profile = open(&root, &identity);
    assert_eq!(cutoff(&profile), None);
}

#[test]
fn blocked_new_growth_after_clock_rollback_cannot_correct_origin() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = open(&root, &identity);
    let state = relay(profile.clone());
    let a = fed_event(&identity, 1, vec![]);
    state.record(&a, 1000, None).unwrap();
    state.record(&scoped(&identity, 2), 100, None).unwrap();
    let age = super::super::super::MAX_RELAY_AGE.as_secs();
    state.next_work_at(age + 200).unwrap();
    let before = state.mailbox_metadata(&a.conversation_id).unwrap().unwrap();
    let child = fed_event(&identity, 2, vec![a.id]);
    assert!(matches!(
        state.record(&child, 150, Some(100)),
        Err(LogError::RelayMailboxUnavailable)
    ));
    assert_eq!(
        state.mailbox_metadata(&a.conversation_id).unwrap().unwrap(),
        before,
        "blocked new growth cannot change origin"
    );
    assert_eq!(state.physical_charge().unwrap().event_count, 2);
}
#[test]
fn stale_policy_cutoff_precedes_corrupt_newer_retirement_progress() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = open(&root, &identity);
    let state = relay(profile.clone());
    let a = scoped(&identity, 1);
    state.record(&a, 0, None).unwrap();
    let view = state
        .policy_view_at(&a.conversation_id, 0)
        .unwrap()
        .unwrap();
    let age = super::super::super::MAX_RELAY_AGE.as_secs();
    state.next_work_at(age).unwrap();
    profile.database().transaction(|c| {
        c.execute("INSERT INTO event_retirement_jobs(domain,conversation,store_uuid,epoch,revision) SELECT e.domain,e.conversation,m.store_uuid,e.destructive_epoch,e.revision FROM event_conversations e CROSS JOIN mesh_database_metadata m WHERE e.domain=?1 AND e.conversation=?2",params![RELAY_EVENTS.0.as_slice(),a.conversation_id.as_bytes().as_slice()])?;Ok(())
    }).unwrap();
    assert!(
        matches!(
            state.validate_policy_view_at(&view, age),
            Err(LogError::StaleCursor)
        ),
        "older cutoff binding must be stale before newer corrupt progress is decoded"
    );
    assert!(
        state.mailbox_metadata_at(&a.conversation_id, age).is_err(),
        "current ordinary reads must retain corruption"
    );
}
#[test]
fn warm_open_rejects_malformed_new_runtime_authority() {
    for sql in [
        "UPDATE relay_runtime SET expired_cutoff=zeroblob(3)",
        "UPDATE relay_runtime SET drain_requested=2",
    ] {
        let root = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let profile = open(&root, &identity);
        profile
            .database()
            .transaction(|c| {
                c.execute_batch("PRAGMA ignore_check_constraints=1")?;
                c.execute_batch(sql)?;
                c.execute_batch("PRAGMA ignore_check_constraints=0")?;
                Ok(())
            })
            .unwrap();
        assert!(
            IndexedRelayState::from_profile(profile, RelayAdmission::LegacyCompatible).is_err(),
            "warm role open must reject malformed new runtime authority: {sql}"
        );
    }
}

#[test]
fn stale_origin_binding_precedes_corrupt_progress_but_current_view_reports_corruption() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = open(&root, &identity);
    let state = relay(profile.clone());
    let a = fed_event(&identity, 1, vec![]);
    state.record(&a, 1000, None).unwrap();
    let old = state
        .policy_view_at(&a.conversation_id, 1000)
        .unwrap()
        .unwrap();
    state.record(&a, 1000, Some(900)).unwrap();
    let current = state
        .policy_view_at(&a.conversation_id, 1000)
        .unwrap()
        .unwrap();
    let child = fed_event(&identity, 2, vec![a.id]);
    state.record(&child, 1000, None).unwrap();
    state.validate_policy_view_at(&current, 1000).unwrap();
    profile.database().transaction(|c| {
        c.execute("INSERT INTO event_retirement_jobs(domain,conversation,store_uuid,epoch,revision) SELECT e.domain,e.conversation,m.store_uuid,e.destructive_epoch,e.revision FROM event_conversations e CROSS JOIN mesh_database_metadata m WHERE e.domain=?1 AND e.conversation=?2",params![RELAY_EVENTS.0.as_slice(),a.conversation_id.as_bytes().as_slice()])?;Ok(())
    }).unwrap();
    assert!(matches!(
        state.validate_policy_view_at(&old, 1000),
        Err(LogError::StaleCursor)
    ));
    assert!(
        matches!(
            state.validate_policy_view_at(&current, 1000),
            Err(LogError::CorruptFile(_))
        ),
        "current binding must retain authoritative corrupt-progress error"
    );
}

#[test]
fn permanent_admission_exhaustion_at_full_envelope_never_requests_destructive_drain() {
    for sql in [
        "UPDATE event_domains SET next_commit=9223372036854775807",
        "UPDATE event_domains SET source_revision=9223372036854775807",
        "UPDATE event_conversations SET revision=9223372036854775807",
        "UPDATE event_conversations SET event_count=9223372036854775807",
        "UPDATE event_conversations SET estimated_bytes=9223372036854775807",
        "UPDATE event_conversations SET stored_bytes=9223372036854775807",
        "UPDATE relay_runtime SET next_touch=x'FFFFFFFFFFFFFFFF'",
    ] {
        let root = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let profile = open(&root, &identity);
        let mut state = relay(profile.clone());
        state.set_limits_for_test(1, 0, 1_000_000, 0).unwrap();
        let a = event(&identity, 1, vec![]);
        state.record(&a, 100, None).unwrap();
        let child = event(&identity, 2, vec![a.id]);
        profile
            .database()
            .transaction(|c| {
                c.execute_batch(sql)?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            state.record(&a, 100, None).unwrap(),
            AppendOutcome::Duplicate,
            "validated duplicates do not need growth counters/touch"
        );
        let charge = state.physical_charge().unwrap();
        let metadata = state.mailbox_metadata(&a.conversation_id).unwrap();
        let watch = state.subscribe_changes();
        let generation = *watch.borrow();
        assert!(
            matches!(
                state.record(&child, 100, None),
                Err(LogError::RepositoryCounterExhausted)
            ),
            "permanent exhaustion must not be pressure: {sql}"
        );
        assert_eq!(state.physical_charge().unwrap(), charge);
        assert_eq!(
            state.mailbox_metadata(&a.conversation_id).unwrap(),
            metadata
        );
        assert_eq!(*watch.borrow(), generation);
        assert!(
            !state.next_work_at(100).unwrap().drain_requested,
            "exhaustion must not schedule eviction: {sql}"
        );
        assert!(state.retained_event_at(&child.id, 100).unwrap().is_none());
    }
}

#[test]
fn work_probe_rejects_selected_role_repository_lifecycle_mismatch() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = open(&root, &identity);
    let state = relay(profile.clone());
    let e = scoped(&identity, 1);
    state.record(&e, 0, None).unwrap();
    // A raw repository transition is deliberately not an alternate role owner.
    profile
        .events()
        .unwrap()
        .begin_retirement(&e.conversation_id)
        .unwrap();
    let age = super::super::super::MAX_RELAY_AGE.as_secs();
    assert!(
        state.next_work_at(age).is_err(),
        "an indexed expiry candidate must validate mirrored lifecycle authority"
    );
}
