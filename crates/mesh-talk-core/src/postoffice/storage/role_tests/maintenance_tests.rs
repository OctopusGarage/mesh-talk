use super::super::role::maintenance::RelayMaintenanceBudget;
use super::policy_tests::scoped;
use super::*;
fn assert_physical(profile: &ProfileStorage) {
    profile.database().read(|c| {
        let actual:(i64,i64)=c.query_row("SELECT count(*),coalesce(sum(ciphertext_bytes+32*parent_count+256),0) FROM retained_events WHERE domain=?1",[RELAY_EVENTS.0.as_slice()],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let charged:(i64,i64)=c.query_row("SELECT event_count,estimated_bytes FROM event_domains WHERE domain=?1",[RELAY_EVENTS.0.as_slice()],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert_eq!(actual,charged);assert!(c.prepare("PRAGMA foreign_key_check")?.query([])?.next()?.is_none());Ok(())
    }).unwrap();
}
#[test]
fn begin_gc_and_real_commit_faults_preserve_coupled_role_authority_on_reopen() {
    let age = super::super::super::MAX_RELAY_AGE.as_secs();
    for point in ["begin", "reclaim", "commit"] {
        let root = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let profile = open(&root, &identity);
        let state = relay(profile.clone());
        let e = scoped(&identity, 1);
        state.record(&e, 0, None).unwrap();
        state.next_work_at(age).unwrap();
        let watch = state.subscribe_changes();
        let generation = *watch.borrow();
        CUT.with(|v| v.set(Some(point)));
        assert!(
            state
                .maintenance_step_at(age, RelayMaintenanceBudget::tiny())
                .is_err(),
            "{point}"
        );
        CUT.with(|v| v.set(None));
        assert_eq!(*watch.borrow(), generation);
        if point == "commit" {
            assert!(state.physical_charge().is_err());
        }
        drop(state);
        drop(profile);
        let profile = open(&root, &identity);
        let state = relay(profile.clone());
        let m = state.mailbox_metadata(&e.conversation_id).unwrap().unwrap();
        assert!(!m.retained.retiring);
        assert_eq!(m.retained.count, 1);
        assert_physical(&profile);
        assert!(matches!(
            state.retained_event_at(&e.id, age),
            Err(LogError::RelayMailboxUnavailable)
        ));
    }
}
#[test]
fn finalization_and_arrival_removal_are_atomic_and_old_token_cannot_erase_recreation() {
    let age = super::super::super::MAX_RELAY_AGE.as_secs();
    for point in ["finalize", "commit"] {
        let root = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let profile = open(&root, &identity);
        let state = relay(profile.clone());
        let e = scoped(&identity, 1);
        state.record(&e, 0, None).unwrap();
        let mut ready = false;
        for _ in 0..100 {
            let p = state
                .maintenance_step_at(age, RelayMaintenanceBudget::tiny())
                .unwrap();
            assert_physical(&profile);
            let reclaim = p.reclaim.unwrap();
            assert!(!reclaim.complete);
            if reclaim.phase == 7 {
                ready = true;
                break;
            }
        }
        assert!(ready);
        assert_eq!(state.physical_charge().unwrap().event_count, 0);
        assert!(matches!(
            state.durable_have_at(&e.conversation_id, &e.id, age),
            Err(LogError::ConversationRetiring)
        ));
        let watch = state.subscribe_changes();
        let generation = *watch.borrow();
        CUT.with(|v| v.set(Some(point)));
        assert!(
            state
                .maintenance_step_at(age, RelayMaintenanceBudget::tiny())
                .is_err()
        );
        CUT.with(|v| v.set(None));
        assert_eq!(*watch.borrow(), generation);
        drop(state);
        drop(profile);
        let profile = open(&root, &identity);
        let state = relay(profile.clone());
        assert!(
            state
                .mailbox_metadata(&e.conversation_id)
                .unwrap()
                .unwrap()
                .retained
                .retiring
        );
        assert_physical(&profile);
        let repo = profile.events().unwrap();
        let old = repo.resume_retirement(&e.conversation_id).unwrap().unwrap();
        assert!(
            state
                .maintenance_step_at(age, RelayMaintenanceBudget::tiny())
                .unwrap()
                .reclaim
                .unwrap()
                .complete
        );
        assert!(
            state
                .mailbox_metadata(&e.conversation_id)
                .unwrap()
                .is_none()
        );
        state.record(&e, age + 1, None).unwrap();
        assert!(matches!(
            repo.reclaim_retirement(&old, Default::default()),
            Err(LogError::StaleCursor)
        ));
        assert!(
            state
                .durable_have_at(&e.conversation_id, &e.id, age + 1)
                .unwrap()
        );
        assert_physical(&profile);
    }
}
#[test]
fn capacity_request_faults_do_not_insert_or_notify_and_true_commit_failure_faults_handle() {
    for point in ["drain", "commit"] {
        let root = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let profile = open(&root, &identity);
        let mut state = relay(profile.clone());
        state.set_limits_for_test(1, 0, 1_000_000, 0).unwrap();
        state.record(&scoped(&identity, 1), 100, None).unwrap();
        let e = scoped(&identity, 2);
        let watch = state.subscribe_changes();
        let generation = *watch.borrow();
        CUT.with(|v| v.set(Some(point)));
        let error = state.record(&e, 100, None).unwrap_err();
        CUT.with(|v| v.set(None));
        assert!(!matches!(error, LogError::RelayCapacityUnavailable));
        assert_eq!(*watch.borrow(), generation);
        if point == "commit" {
            assert!(state.physical_charge().is_err());
        }
        drop(state);
        drop(profile);
        let profile = open(&root, &identity);
        let state = relay(profile.clone());
        assert!(!state.next_work_at(100).unwrap().drain_requested);
        assert_physical(&profile);
        assert!(state.retained_event_at(&e.id, 100).unwrap().is_none());
    }
}
#[test]
fn expired_scopes_precede_lru_and_many_mailboxes_do_not_over_retire() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = open(&root, &identity);
    let mut state = relay(profile.clone());
    state.set_limits_for_test(63, 1, 1_000_000, 0).unwrap();
    for key in 1..=64 {
        state.record(&scoped(&identity, key), 100, None).unwrap();
    }
    assert!(matches!(
        state.record(&scoped(&identity, 65), 100, None),
        Err(LogError::RelayCapacityUnavailable)
    ));
    for _ in 0..4 {
        state
            .maintenance_step_at(100, RelayMaintenanceBudget::tiny())
            .unwrap();
        assert_physical(&profile);
    }
    profile
        .database()
        .read(|c| {
            let pending: i64 = c.query_row(
                "SELECT count(*) FROM relay_mailboxes WHERE pending=1",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(pending, 1);
            let retiring: i64 = c.query_row(
                "SELECT count(*) FROM event_conversations WHERE lifecycle=1",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(retiring, 1);
            Ok(())
        })
        .unwrap();
    assert_eq!(state.physical_charge().unwrap().event_count, 64);
    let root = tempfile::tempdir().unwrap();
    let profile = open(&root, &identity);
    let mut state = relay(profile.clone());
    state.set_limits_for_test(1, 1, 1_000_000, 0).unwrap();
    let recent = scoped(&identity, 1);
    let expired = scoped(&identity, 2);
    state.record(&recent, 1000, None).unwrap();
    state.record(&expired, 0, None).unwrap();
    assert!(state.record(&scoped(&identity, 3), 1000, None).is_err());
    let age = super::super::super::MAX_RELAY_AGE.as_secs();
    state
        .maintenance_step_at(age, RelayMaintenanceBudget::tiny())
        .unwrap();
    assert!(
        !state
            .mailbox_metadata(&recent.conversation_id)
            .unwrap()
            .unwrap()
            .retained
            .retiring
    );
    assert!(
        state
            .mailbox_metadata(&expired.conversation_id)
            .unwrap()
            .unwrap()
            .retained
            .retiring
    );
}
#[test]
fn oversized_corrupt_body_is_not_decoded_and_reports_atomic_gc_exception() {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = open(&root, &identity);
    let state = relay(profile.clone());
    let e = Event::new(
        &identity,
        ConversationId::new([1; 32]),
        1,
        vec![],
        1,
        1,
        EventKind::Message,
        vec![7; 8 * 1024 * 1024],
    );
    state.record(&e, 0, None).unwrap();
    profile
        .database()
        .transaction(|c| {
            c.execute("UPDATE retained_events SET body=zeroblob(length(body))", [])?;
            Ok(())
        })
        .unwrap();
    let age = super::super::super::MAX_RELAY_AGE.as_secs();
    assert!(matches!(
        state.retained_event_at(&e.id, age),
        Err(LogError::RelayMailboxUnavailable)
    ));
    let mut oversized = false;
    let mut completed = false;
    for _ in 0..100 {
        let p = state
            .maintenance_step_at(age, RelayMaintenanceBudget::tiny())
            .unwrap();
        assert_physical(&profile);
        if let Some(reclaim) = p.reclaim {
            if let Some(bytes) = reclaim.oversized_body_bytes {
                assert!(bytes > 8 * 1024 * 1024);
                oversized = true;
            }
            if reclaim.complete {
                completed = true;
                break;
            }
        }
    }
    assert!(oversized);
    assert!(completed);
    assert_eq!(state.physical_charge().unwrap().event_count, 0);
}
