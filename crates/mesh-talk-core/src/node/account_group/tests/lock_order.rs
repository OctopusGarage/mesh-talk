//! A privacy writer must never leave an account-role reader holding the public log.
use super::*;

fn privacy_contention_keeps_public_log_available(local_observation: bool) {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let owner = node(&dir.path().join("owner"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let genesis = legacy_group(&owner, &external);
    let group = genesis.conversation_id;
    install(&owner, &genesis);
    let privacy_writer = owner.privacy.state.write().unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let task_owner = owner.clone();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        if local_observation {
            task_owner
                .account_group_local_observation(group, None)
                .unwrap()
                .observation
                .role
        } else {
            task_owner.account_group_access(group).unwrap().role
        }
    });
    started_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    // The writer is deliberately retained while the role reader reaches its
    // privacy snapshot. Existing sync readers acquire privacy before this log;
    // retaining the log here would form their real production lock cycle.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    let mut retained_log = false;
    while std::time::Instant::now() < deadline {
        if owner.log.try_lock().is_err() {
            retained_log = true;
            break;
        }
        std::thread::yield_now();
    }
    // Release before asserting, so a failed regression never strands a worker.
    drop(privacy_writer);
    let role = worker.join().unwrap();
    assert_eq!(role, AccountGroupRole::Owner);
    assert!(
        !retained_log,
        "account role lookup retained the public log while waiting for privacy"
    );
}

#[test]
fn local_observation_waits_for_privacy_without_holding_the_public_log() {
    privacy_contention_keeps_public_log_available(true);
}

#[test]
fn account_access_waits_for_privacy_without_holding_the_public_log() {
    privacy_contention_keeps_public_log_available(false);
}
