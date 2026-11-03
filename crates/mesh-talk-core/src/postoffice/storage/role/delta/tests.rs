use super::*;
use crate::identity::device::DeviceIdentity;
use crate::storage::profile::ProfileStorageLease;

fn fixture() -> (tempfile::TempDir, IndexedRelayState, DeviceIdentity) {
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let profile = ProfileStorage::open_relay(
        ProfileStorageLease::acquire(root.path()).unwrap(),
        &identity.public(),
        &root.path().join("relay.log"),
        "pw",
        10,
    )
    .unwrap();
    let state = IndexedRelayState::from_profile(profile, RelayAdmission::LegacyCompatible).unwrap();
    (root, state, identity)
}

fn event(identity: &DeviceIdentity, conversation: ConversationId, seq: u64) -> Event {
    Event::new(
        identity,
        conversation,
        seq,
        vec![],
        seq,
        seq,
        crate::eventlog::EventKind::Message,
        vec![42; 128],
    )
}

#[test]
fn mailbox_delta_idle_ignores_other_mailboxes_and_reads_no_bodies() {
    let (_root, state, identity) = fixture();
    let conv = ConversationId::new([1; 32]);
    let first = event(&identity, conv, 1);
    state.record(&first, 10, None).unwrap();
    let page = state.mailbox_delta_page_at(&conv, None, 2, 10).unwrap();
    assert_eq!(page.ids, [first.id]);
    assert!(page.complete);
    let unrelated = event(&identity, ConversationId::new([2; 32]), 1);
    state.record(&unrelated, 11, None).unwrap();
    let counts = state.body_read_counts();
    let changes = state
        .profile
        .database()
        .read(|c| Ok(c.total_changes()))
        .unwrap();
    let idle = state
        .mailbox_delta_page_at(&conv, Some(&page.cursor), 2, 11)
        .unwrap();
    assert_eq!(idle.cursor, page.cursor);
    assert!(idle.ids.is_empty() && idle.complete && !idle.reset);
    assert_eq!(state.body_read_counts(), counts);
    assert_eq!(
        state
            .profile
            .database()
            .read(|c| Ok(c.total_changes()))
            .unwrap(),
        changes
    );
}

#[test]
fn mailbox_delta_finite_window_finishes_before_concurrent_append() {
    let (_root, state, identity) = fixture();
    let conv = ConversationId::new([1; 32]);
    let events: Vec<_> = (1..=3).map(|n| event(&identity, conv, n)).collect();
    for e in &events[..2] {
        state.record(e, 10, None).unwrap();
    }
    let first = state.mailbox_delta_page_at(&conv, None, 1, 10).unwrap();
    assert_eq!(first.ids, [events[0].id]);
    assert!(!first.complete);
    state.record(&events[2], 11, None).unwrap();
    let second = state
        .mailbox_delta_page_at(&conv, Some(&first.cursor), 1, 11)
        .unwrap();
    assert_eq!(second.ids, [events[1].id]);
    assert!(second.complete);
    let delta = state
        .mailbox_delta_page_at(&conv, Some(&second.cursor), 1, 11)
        .unwrap();
    assert_eq!(delta.ids, [events[2].id]);
    assert!(delta.complete && !delta.reset);
}

#[test]
fn mailbox_delta_resets_foreign_and_destructive_prefixes_and_checks_expiry() {
    let (_root, state, identity) = fixture();
    let conv = ConversationId::new([1; 32]);
    let e = event(&identity, conv, 1);
    state.record(&e, 10, None).unwrap();
    let page = state.mailbox_delta_page_at(&conv, None, 2, 10).unwrap();
    let mut foreign = page.cursor.clone();
    foreign.store_uuid[0] ^= 1;
    assert!(
        state
            .mailbox_delta_page_at(&conv, Some(&foreign), 2, 10)
            .unwrap()
            .reset
    );
    state.profile.database().transaction(|c| {
        c.execute("UPDATE event_conversations SET destructive_epoch=destructive_epoch+1 WHERE domain=?1 AND conversation=?2", params![RELAY_EVENTS.0.as_slice(),conv.as_bytes().as_slice()])?;
        Ok(())
    }).unwrap();
    assert!(
        state
            .mailbox_delta_page_at(&conv, Some(&page.cursor), 2, 10)
            .unwrap()
            .reset
    );
    assert!(
        state
            .mailbox_delta_page_at(
                &conv,
                Some(&page.cursor),
                2,
                10 + super::super::super::super::MAX_RELAY_AGE.as_secs()
            )
            .is_err()
    );
}

#[test]
fn mailbox_delta_exact_fetch_rejects_wrong_scope_and_small_budget_before_body_read() {
    let (_root, state, identity) = fixture();
    let conv = ConversationId::new([1; 32]);
    let e = event(&identity, conv, 1);
    let other = event(&identity, ConversationId::new([2; 32]), 1);
    state.record(&e, 10, None).unwrap();
    state.record(&other, 10, None).unwrap();
    let page = state.mailbox_delta_page_at(&conv, None, 2, 10).unwrap();
    let counts = state.body_read_counts();
    assert!(
        state
            .mailbox_delta_events_at(&conv, &page.cursor, &[other.id], 60000, 10)
            .is_err()
    );
    assert!(
        state
            .mailbox_delta_events_at(&conv, &page.cursor, &[e.id], 1, 10)
            .is_err()
    );
    assert_eq!(state.body_read_counts(), counts);
    assert_eq!(
        state
            .mailbox_delta_events_at(&conv, &page.cursor, &[e.id], 60000, 10)
            .unwrap(),
        [e]
    );
}

#[test]
fn mailbox_delta_reopen_preserves_prefix_and_retirement_recreation_resets_it() {
    use super::super::maintenance::RelayMaintenanceBudget;
    let (root, state, identity) = fixture();
    let conv = ConversationId::new([1; 32]);
    let e = event(&identity, conv, 1);
    state.record(&e, 10, None).unwrap();
    let page = state.mailbox_delta_page_at(&conv, None, 2, 10).unwrap();
    drop(state);
    let profile = ProfileStorage::open_relay(
        ProfileStorageLease::acquire(root.path()).unwrap(),
        &identity.public(),
        &root.path().join("relay.log"),
        "pw",
        11,
    )
    .unwrap();
    let state = IndexedRelayState::from_profile(profile, RelayAdmission::LegacyCompatible).unwrap();
    let reopened = state
        .mailbox_delta_page_at(&conv, Some(&page.cursor), 2, 11)
        .unwrap();
    assert_eq!(reopened.cursor, page.cursor);
    assert!(reopened.ids.is_empty() && !reopened.reset);
    let now = 10 + super::super::super::super::MAX_RELAY_AGE.as_secs();
    for _ in 0..128 {
        if !state.next_work_at(now).unwrap().has_work() {
            break;
        }
        state
            .maintenance_step_at(now, RelayMaintenanceBudget::default())
            .unwrap();
    }
    assert!(!state.next_work_at(now).unwrap().has_work());
    let replacement = event(&identity, conv, 2);
    state.record(&replacement, now + 1, None).unwrap();
    let fresh = state
        .mailbox_delta_page_at(&conv, Some(&page.cursor), 2, now + 1)
        .unwrap();
    assert!(fresh.reset && fresh.complete);
    assert_eq!(fresh.ids, [replacement.id]);
    assert!(
        state
            .mailbox_delta_events_at(&conv, &page.cursor, &[e.id], 60000, now + 1)
            .is_err()
    );
}

#[test]
fn mailbox_delta_selected_fetch_cannot_read_past_delivered_finite_prefix() {
    let (_root, state, identity) = fixture();
    let conv = ConversationId::new([1; 32]);
    let first = event(&identity, conv, 1);
    let second = event(&identity, conv, 2);
    state.record(&first, 10, None).unwrap();
    state.record(&second, 10, None).unwrap();
    let page = state.mailbox_delta_page_at(&conv, None, 1, 10).unwrap();
    assert!(!page.complete);
    let third = event(&identity, conv, 3);
    state.record(&third, 11, None).unwrap();
    let counts = state.body_read_counts();
    for id in [second.id, third.id] {
        assert!(
            state
                .mailbox_delta_events_at(&conv, &page.cursor, &[id], 60000, 11)
                .is_err()
        );
    }
    assert_eq!(state.body_read_counts(), counts);
    assert_eq!(
        state
            .mailbox_delta_events_at(&conv, &page.cursor, &[first.id], 60000, 11)
            .unwrap(),
        [first]
    );
}

#[test]
fn mailbox_delta_covering_seek_work_stays_bounded_at_10k_and_100k() {
    use rusqlite::StatementStatus;
    for count in [10_000i64, 100_000] {
        let (_root, state, identity) = fixture();
        let conv = ConversationId::new([1; 32]);
        let root = event(&identity, conv, 1);
        state.record(&root, 10, None).unwrap();
        let mut cursor = state
            .mailbox_delta_page_at(&conv, None, 2, 10)
            .unwrap()
            .cursor;
        // Trusted index fixture: synthetic rows are never decoded. Exercise
        // the production seek without doing 100k signatures or body reads.
        state.profile.database().transaction(|c| {
            let mut stmt = c.prepare("INSERT INTO retained_events(domain,id,conversation,author,seq,lamport,wall_clock,kind,parent_count,encoded_bytes,ciphertext_bytes,signature_bytes,stored_bytes,body,commit_ordinal) SELECT domain,?1,conversation,author,?2,lamport,wall_clock,kind,parent_count,encoded_bytes,ciphertext_bytes,signature_bytes,stored_bytes,body,?3 FROM retained_events WHERE domain=?4 AND id=?5")?;
            for ordinal in 1..count {
                let mut id = [0;32]; id[..8].copy_from_slice(&ordinal.to_be_bytes());
                stmt.execute(params![id.as_slice(), (ordinal as u64 + 1).to_be_bytes().as_slice(), ordinal, RELAY_EVENTS.0.as_slice(),root.id.as_bytes().as_slice()])?;
            }
            c.execute("UPDATE event_conversations SET revision=?1,event_count=?1", [count])?;
            c.execute("UPDATE event_domains SET next_commit=?1,event_count=?1", [count])?;
            Ok(())
        }).unwrap();
        cursor.after = count - 1;
        cursor.revision = count - 1;
        let counts = state.body_read_counts();
        let page = state
            .mailbox_delta_page_at(&conv, Some(&cursor), 1, 10)
            .unwrap();
        assert_eq!(page.ids.len(), 1);
        assert!(page.complete && !page.reset);
        assert_eq!(state.body_read_counts(), counts);
        state
            .profile
            .database()
            .read(|c| {
                let arguments = params![
                    RELAY_EVENTS.0.as_slice(),
                    conv.as_bytes().as_slice(),
                    count - 1,
                    count,
                    2
                ];
                let plan = {
                    let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {SELECT_DELTA}"))?;
                    let rows = stmt.query_map(arguments, |r| r.get::<_, String>(3))?;
                    rows.collect::<Result<Vec<_>, _>>()?
                };
                assert!(
                    plan.iter()
                        .any(|p| p.contains("COVERING INDEX retained_events_commit")),
                    "{plan:?}"
                );
                assert!(!plan.iter().any(|p| p.contains("TEMP")), "{plan:?}");
                let mut stmt = c.prepare(SELECT_DELTA)?;
                {
                    let mut rows = stmt.query(arguments)?;
                    while rows.next()?.is_some() {}
                }
                assert!(
                    stmt.get_status(StatementStatus::VmStep) < 100,
                    "count={count}"
                );
                assert_eq!(stmt.get_status(StatementStatus::FullscanStep), 0);
                assert_eq!(stmt.get_status(StatementStatus::Sort), 0);
                Ok(())
            })
            .unwrap();
    }
}
