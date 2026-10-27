//! One owned bounded maintenance loop. No spawned blocking work can outlive it.
use super::IndexedPostOffice;
use crate::{
    eventlog::LogError,
    postoffice::storage::{
        federation,
        role::{inventory::preparation::DigestWorker, maintenance::RelayMaintenanceBudget},
    },
    storage::{
        database::{EncryptedDatabase, PhysicalReclaimBudget, database_busy},
        profile::context::ArchiveCleanupOutcome,
    },
};
use std::time::{Duration, Instant};
use tokio::sync::watch;

#[cfg(test)]
thread_local! {
static PHYSICAL_INSPECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
static SLICE_TIMES: std::cell::RefCell<Vec<(Instant,Instant)>> = const { std::cell::RefCell::new(Vec::new()) };}

const MIN_SPACING: Duration = Duration::from_millis(10);
fn pause_for(elapsed: Duration, failures: u32) -> Duration {
    let duty_pause = elapsed.saturating_mul(3).max(MIN_SPACING);
    if failures == 0 {
        duty_pause
    } else {
        duty_pause.max(
            Duration::from_millis(100u64 << failures.saturating_sub(1).min(6))
                .min(Duration::from_secs(5)),
        )
    }
}

// Hysteresis amortizes tiny session deletes. Each eligible physical slice
// returns to all other maintenance classes and the measured-duty pacing gate.
const PHYSICAL_START_PAGES: u64 = 256;
const PHYSICAL_STOP_PAGES: u64 = 64;
const PHYSICAL_COOLDOWN: Duration = Duration::from_millis(100);
#[derive(Default)]
struct PhysicalCleanup {
    active: bool,
    inspected_main: bool,
    checkpoint_pending: bool,
    failures: u32,
    next_eligible: Option<tokio::time::Instant>,
}
impl PhysicalCleanup {
    fn step(&mut self, database: &EncryptedDatabase) -> Result<(), LogError> {
        if self
            .next_eligible
            .is_some_and(|at| at > tokio::time::Instant::now())
        {
            return Ok(());
        }
        #[cfg(test)]
        PHYSICAL_INSPECTIONS.with(|v| v.set(v.get() + 1));
        let result = (|| {
            if !self.inspected_main {
                self.checkpoint_pending = database.main_file_exceeds_page_estimate()?;
                self.inspected_main = true;
            }
            let stats = database.physical_stats()?;
            if stats.freelist_count <= PHYSICAL_STOP_PAGES {
                self.active = false;
            } else if stats.freelist_count >= PHYSICAL_START_PAGES {
                self.active = true;
            }
            if !self.active && !self.checkpoint_pending {
                self.failures = 0;
                self.next_eligible = None;
                return Ok(());
            }
            if self.active {
                let progress = database.reclaim_pages(PhysicalReclaimBudget::default())?;
                log::debug!(
                    "indexed relay physical cleanup: {} iterations, free pages {} -> {}, SQL pages {} -> {}",
                    progress.yielded_steps,
                    progress.before.freelist_count,
                    progress.after.freelist_count,
                    progress.before.page_count,
                    progress.after.page_count
                );
                self.active = progress.yielded_steps > 0
                    && progress.after.freelist_count > PHYSICAL_STOP_PAGES;
                self.checkpoint_pending |= progress.yielded_steps > 0;
            }
            if self.checkpoint_pending {
                // A completed PASSIVE transfer permits main truncation. It does
                // not prove filesystem block release or shrink retained WAL bytes.
                let progress = database.passive_checkpoint()?;
                self.checkpoint_pending = !progress.complete();
                if self.checkpoint_pending {
                    self.failures = self.failures.saturating_add(1);
                    self.next_eligible = Some(
                        tokio::time::Instant::now() + pause_for(Duration::ZERO, self.failures),
                    );
                    log::debug!("indexed relay physical checkpoint deferred: {progress:?}");
                    return Ok(());
                }
            }
            self.failures = 0;
            self.next_eligible = self
                .active
                .then(|| tokio::time::Instant::now() + PHYSICAL_COOLDOWN);
            Ok(())
        })();
        match result {
            Err(error) if !database.requires_reopen() && database_busy(&error) => {
                self.failures = self.failures.saturating_add(1);
                self.next_eligible =
                    Some(tokio::time::Instant::now() + pause_for(Duration::ZERO, self.failures));
                log::warn!("indexed relay physical cleanup deferred: {error}");
                Ok(())
            }
            result => result,
        }
    }
}

impl IndexedPostOffice {
    /// Faulted/poisoned storage is unusable until reopen. No error string parsing.
    pub fn requires_reopen(&self) -> bool {
        self.state.profile().database().requires_reopen()
    }

    /// The caller owns and polls this future through shutdown. Each synchronous
    /// transaction completes before its next await, so cancellation drains the
    /// admitted slice and retains the OS lease until that slice has returned.
    pub async fn run_maintenance(&self, mut stop: watch::Receiver<bool>) -> Result<(), LogError> {
        let mut visible = self.state.subscribe_changes();
        let mut work = self.state.subscribe_work();
        let profile = self.state.profile();
        let mut cancelled = profile.federation_cancellations().subscribe_changes();
        let mut hashes = DigestWorker::default();
        let mut failures = 0u32;
        let mut physical = PhysicalCleanup::default();
        let mut archive_failures = 0u32;
        let mut archive_retry_at: Option<tokio::time::Instant> = None;
        loop {
            if *stop.borrow_and_update() {
                return Ok(());
            }
            // Mark generations BEFORE inspecting. A concurrent publish during
            // inspection stays unread and wakes the wait below.
            visible.borrow_and_update();
            work.borrow_and_update();
            cancelled.borrow_and_update();
            let started = Instant::now();
            let now = super::network::unix_now();
            let slice = (|| {
                let role = self.state.next_work_at(now)?;
                if role.has_work() {
                    self.state
                        .maintenance_step_at(now, RelayMaintenanceBudget::default())?;
                }
                let digest = self.state.digest_work_pending()?;
                let digest_pending = if digest {
                    hashes.step_at(&self.state, now)?.pending
                } else {
                    false
                };
                let mut staging_pending = false;
                if profile.cleanup_pending()? {
                    let archive_due =
                        archive_retry_at.is_none_or(|at| at <= tokio::time::Instant::now());
                    let progress = profile.cleanup_step(archive_due)?;
                    staging_pending = progress.staging_pending;
                    match progress.archive {
                        ArchiveCleanupOutcome::Complete => {
                            archive_failures = 0;
                            archive_retry_at = None;
                        }
                        ArchiveCleanupOutcome::Waiting => {}
                        ArchiveCleanupOutcome::Deferred(error) => {
                            archive_failures = archive_failures.saturating_add(1);
                            archive_retry_at = Some(
                                tokio::time::Instant::now()
                                    + pause_for(Duration::ZERO, archive_failures),
                            );
                            log::warn!("indexed relay archive cleanup deferred: {error}");
                        }
                    }
                } else {
                    archive_failures = 0;
                    archive_retry_at = None;
                }
                // Archive-only IO/BUSY cannot skip independent cancellation work.
                let session_progress = federation::cleanup_step(profile, now)?;
                let pending =
                    role.has_work() || digest_pending || staging_pending || session_progress;
                let expiry = role.next_expiry.and_then(|utc| {
                    Instant::now()
                        .checked_add(Duration::from_secs(utc.saturating_sub(now).min(86_400)))
                });
                let deadline = match (expiry, profile.federation_cancellations().next_deadline()) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                Ok::<_, LogError>((pending, deadline))
            })();
            let (pending, deadline) = match slice {
                Ok(result) => {
                    failures = 0;
                    result
                }
                Err(error) => {
                    if self.requires_reopen() {
                        return Err(error);
                    }
                    // Only bounded local retry outcomes may keep this owner.
                    if !matches!(
                        error,
                        LogError::StaleCursor
                            | LogError::RelayCapacityUnavailable
                            | LogError::ConversationRetiring
                            | LogError::RelayMailboxUnavailable
                            | LogError::Database(_)
                            | LogError::Io(_)
                    ) {
                        return Err(error);
                    }
                    failures = failures.saturating_add(1);
                    log::warn!("indexed relay maintenance: {error}");
                    (true, None)
                }
            };
            // Physical contention has its own deadline; it cannot short-circuit
            // role, digest, staging, archive or session work above. All other
            // physical DB errors propagate, including authority validation.
            physical.step(profile.database())?;
            #[cfg(test)]
            SLICE_TIMES.with(|v| v.borrow_mut().push((started, Instant::now())));
            let deadline = match (
                deadline.map(tokio::time::Instant::from_std),
                archive_retry_at,
            ) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let deadline = match (deadline, physical.next_eligible) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            // Self-notifications never bypass pacing. Cancellation alone can
            // interrupt the spacing/duty/error-backoff gate.
            tokio::select! {
                biased;
                result = stop.changed() => { if result.is_err() || *stop.borrow() { return Ok(()); } }
                _ = tokio::time::sleep(pause_for(started.elapsed(),failures)) => {}
            }
            if pending {
                continue;
            }
            tokio::select! {
                biased;
                result = stop.changed() => { if result.is_err() || *stop.borrow() { return Ok(()); } }
                _ = visible.changed() => {}
                _ = work.changed() => {}
                _ = cancelled.changed() => {}
                _ = async { match deadline { Some(at) => tokio::time::sleep_until(at).await, None => std::future::pending().await } } => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{identity::device::DeviceIdentity, storage::profile::ProfileStorageLease};
    fn office(root: &tempfile::TempDir) -> IndexedPostOffice {
        IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(root.path()).unwrap(),
            &root.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap()
    }
    #[test]
    fn physical_zero_frame_completion_does_not_retry_main_padding() {
        use crate::storage::database::observations::PASSIVE_ATTEMPTS;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profile.db");
        let db = EncryptedDatabase::open(&path, "pw", b"owner").unwrap();
        db.checkpoint().unwrap();
        let original = std::fs::metadata(&path).unwrap().len();
        // Test-only trailing extent: no logical free-page work or WAL frames.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(original + 4096)
            .unwrap();
        assert!(db.main_file_exceeds_page_estimate().unwrap());
        PASSIVE_ATTEMPTS.with(|v| v.borrow_mut().clear());
        let mut cleanup = PhysicalCleanup::default();
        cleanup.step(&db).unwrap();
        assert!(!cleanup.checkpoint_pending);
        assert!(cleanup.next_eligible.is_none());
        assert_eq!(PASSIVE_ATTEMPTS.with(|v| v.borrow().len()), 1);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            original + 4096,
            "zero transferred frames do not prove filesystem shrink"
        );
        cleanup.step(&db).unwrap();
        assert_eq!(
            PASSIVE_ATTEMPTS.with(|v| v.borrow().len()),
            1,
            "padding alone must not create a retry loop"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn physical_restart_discovers_small_wal_truncation_and_ignores_ordinary_growth() {
        use crate::storage::database::observations::PASSIVE_ATTEMPTS;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profile.db");
        let db = EncryptedDatabase::open(&path, "pw", b"owner").unwrap();
        db.transaction(|c| {
            c.execute_batch("CREATE TABLE disposable(body BLOB)")?;
            c.execute("INSERT INTO disposable VALUES(zeroblob(2097152))", [])?;
            c.execute("DELETE FROM disposable", [])?;
            Ok(())
        })
        .unwrap();
        db.checkpoint().unwrap();
        let main_before = std::fs::metadata(&path).unwrap().len();
        let reader = EncryptedDatabase::open(&path, "pw", b"owner").unwrap();
        reader
            .read(|c| {
                c.execute_batch("BEGIN")?;
                c.query_row("SELECT count(*) FROM disposable", [], |r| {
                    r.get::<_, i64>(0)
                })?;
                Ok(())
            })
            .unwrap();
        while db.physical_stats().unwrap().freelist_count > PHYSICAL_STOP_PAGES {
            db.reclaim_pages(PhysicalReclaimBudget::default()).unwrap();
        }
        drop(db); // A second live reader prevents last-connection checkpoint.
        let db = EncryptedDatabase::open(&path, "pw", b"owner").unwrap();
        assert!(db.physical_stats().unwrap().freelist_count < PHYSICAL_START_PAGES);
        assert!(db.main_file_exceeds_page_estimate().unwrap());
        PASSIVE_ATTEMPTS.with(|v| v.borrow_mut().clear());
        let mut cleanup = PhysicalCleanup::default();
        cleanup.step(&db).unwrap();
        assert!(!cleanup.active);
        assert!(
            cleanup.checkpoint_pending,
            "partial PASSIVE is unfinished even with busy flag zero"
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), main_before);
        reader
            .read(|c| {
                c.execute_batch("ROLLBACK")?;
                Ok(())
            })
            .unwrap();
        drop(reader);
        tokio::time::sleep_until(cleanup.next_eligible.unwrap()).await;
        cleanup.step(&db).unwrap();
        assert!(!cleanup.checkpoint_pending);
        assert!(std::fs::metadata(&path).unwrap().len() < main_before);
        assert_eq!(PASSIVE_ATTEMPTS.with(|v| v.borrow().len()), 2);
        db.transaction(|c| {
            c.execute("INSERT INTO disposable VALUES(zeroblob(131072))", [])?;
            Ok(())
        })
        .unwrap();
        assert!(!db.main_file_exceeds_page_estimate().unwrap());
        PASSIVE_ATTEMPTS.with(|v| v.borrow_mut().clear());
        PhysicalCleanup::default().step(&db).unwrap();
        assert_eq!(
            PASSIVE_ATTEMPTS.with(|v| v.borrow().len()),
            0,
            "ordinary pending appends cannot trigger a startup checkpoint"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn physical_worker_releases_main_file_without_reader_and_after_reader_retires() {
        for pinned in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let identity = DeviceIdentity::generate();
            let public = identity.public();
            let office = IndexedPostOffice::open_with_lease(
                ProfileStorageLease::acquire(root.path()).unwrap(),
                &root.path().join("relay.log"),
                "pw",
                identity,
            )
            .unwrap();
            while office.state.profile().cleanup_pending().unwrap() {
                office.state.profile().cleanup_step(true).unwrap();
            }
            let db = office.state.profile().database();
            db.transaction(|c| {
                c.execute_batch("CREATE TABLE disposable_pages(body BLOB)")?;
                c.execute("INSERT INTO disposable_pages VALUES(zeroblob(2097152))", [])?;
                c.execute("DELETE FROM disposable_pages", [])?;
                Ok(())
            })
            .unwrap();
            db.checkpoint().unwrap();
            let main_path = root.path().join("profile.db");
            let main_before = std::fs::metadata(&main_path).unwrap().len();
            let reader = pinned.then(|| block_archive_checkpoint(&office, &public));
            let (stop, rx) = watch::channel(false);
            let worker = office.run_maintenance(rx);
            tokio::pin!(worker);
            tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"),_=tokio::time::sleep(Duration::from_secs(2))=>{}}
            let stats = db.physical_stats().unwrap();
            if !pinned {
                assert!(stats.freelist_count <= PHYSICAL_STOP_PAGES);
            }
            if let Some(reader) = reader {
                assert_eq!(
                    std::fs::metadata(&main_path).unwrap().len(),
                    main_before,
                    "snapshot pins main-file truncation"
                );
                reader
                    .read(|c| {
                        c.execute_batch("ROLLBACK")?;
                        Ok(())
                    })
                    .unwrap();
                drop(reader);
                tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"),_=tokio::time::sleep(Duration::from_secs(6))=>{}}
            }
            assert!(db.physical_stats().unwrap().freelist_count <= PHYSICAL_STOP_PAGES);
            let main_after = std::fs::metadata(&main_path).unwrap().len();
            assert!(
                main_after < main_before,
                "background physical completion must release main-file space; pinned={pinned}, SQL={stats:?}, main={main_before}->{main_after}"
            );
            println!(
                "worker pinned={pinned}: main {main_before}->{main_after}, WAL={} bytes",
                std::fs::metadata(root.path().join("profile.db-wal"))
                    .unwrap()
                    .len()
            );
            stop.send_replace(true);
            worker.await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn healthy_watches_progress_without_physical_inspection_until_success_eligibility() {
        use crate::postoffice::storage::federation::{Direction, SessionOwner};
        use crate::storage::database::observations::VACUUM_ATTEMPTS;
        let root = tempfile::tempdir().unwrap();
        let office = office(&root);
        while office.state.profile().cleanup_pending().unwrap() {
            office.state.profile().cleanup_step(true).unwrap();
        }
        let db = office.state.profile().database();
        db.transaction(|c| {
            c.execute_batch("CREATE TABLE disposable_pages(body BLOB)")?;
            c.execute("INSERT INTO disposable_pages VALUES(zeroblob(2097152))", [])?;
            c.execute("DELETE FROM disposable_pages", [])?;
            Ok(())
        })
        .unwrap();
        PHYSICAL_INSPECTIONS.with(|v| v.set(0));
        VACUUM_ATTEMPTS.with(|v| v.borrow_mut().clear());
        let (stop, rx) = watch::channel(false);
        let worker = office.run_maintenance(rx);
        tokio::pin!(worker);
        tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"),_=tokio::time::sleep(Duration::from_millis(1))=>{}}
        let first = VACUUM_ATTEMPTS.with(|v| {
            assert_eq!(v.borrow().len(), 1);
            v.borrow()[0]
        });
        let after_first = db.physical_stats().unwrap();
        assert_eq!(PHYSICAL_INSPECTIONS.with(|v| v.get()), 1);
        db.transaction(|c| {
            c.execute("UPDATE relay_publication SET staging_pending=1", [])?;
            Ok(())
        })
        .unwrap();
        drop(
            SessionOwner::begin(
                office.state.profile().clone(),
                Direction::Inbound,
                super::super::network::unix_now(),
                Instant::now(),
            )
            .unwrap(),
        );
        office
            .state
            .request_preparation_at(super::super::network::unix_now())
            .unwrap();
        tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"),_=tokio::time::sleep_until(first + Duration::from_millis(90))=>{}}
        assert!(
            office
                .state
                .preparation_ready_at(super::super::network::unix_now())
                .unwrap()
        );
        assert!(!office.state.profile().cleanup_pending().unwrap());
        assert_eq!(
            scalar(&office, "SELECT session_rows FROM relay_federation_runtime"),
            0
        );
        assert_eq!(VACUUM_ATTEMPTS.with(|v| v.borrow().len()), 1);
        assert_eq!(
            PHYSICAL_INSPECTIONS.with(|v| v.get()),
            1,
            "healthy watch wakes must not inspect physical scalars before success eligibility"
        );
        let limit = first + Duration::from_secs(2);
        while VACUUM_ATTEMPTS.with(|v| v.borrow().len()) < 2 {
            tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"),_=tokio::time::sleep(Duration::from_millis(10))=>{}}
            assert!(tokio::time::Instant::now() < limit);
        }
        let attempts = VACUUM_ATTEMPTS.with(|v| v.borrow().clone());
        assert!(attempts[1] - attempts[0] >= PHYSICAL_COOLDOWN);
        assert!(PHYSICAL_INSPECTIONS.with(|v| v.get()) >= 2);
        assert!(db.physical_stats().unwrap().freelist_count < after_first.freelist_count);
        stop.send_replace(true);
        worker.await.unwrap();
    }

    #[tokio::test]
    async fn startup_reclaims_a_capped_slice_then_waits_without_idle_writes() {
        use crate::storage::database::observations::VACUUM_STATEMENTS;
        let root = tempfile::tempdir().unwrap();
        let office = office(&root);
        while office.state.profile().cleanup_pending().unwrap() {
            office.state.profile().cleanup_step(true).unwrap();
        }
        let db = office.state.profile().database();
        db.transaction(|c| {
            c.execute_batch("CREATE TABLE disposable_pages(body BLOB)")?;
            c.execute("INSERT INTO disposable_pages VALUES(zeroblob(2097152))", [])?;
            c.execute("DELETE FROM disposable_pages", [])?;
            Ok(())
        })
        .unwrap();
        let before = db.physical_stats().unwrap();
        assert!(before.freelist_count >= 256);
        VACUUM_STATEMENTS.with(|v| v.set(0));
        PHYSICAL_INSPECTIONS.with(|v| v.set(0));
        let (stop, rx) = watch::channel(false);
        let worker = office.run_maintenance(rx);
        tokio::pin!(worker);
        tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"),_=tokio::time::sleep(Duration::from_millis(60))=>{}}
        stop.send_replace(true);
        worker.await.unwrap();
        let after = db.physical_stats().unwrap();
        assert!(
            after.page_count < before.page_count,
            "startup free pages must receive a slice"
        );
        assert_eq!(before.freelist_count - after.freelist_count, 64);
        assert_eq!(VACUUM_STATEMENTS.with(|v| v.get()), 1);
        assert_eq!(
            PHYSICAL_INSPECTIONS.with(|v| v.get()),
            1,
            "successful cooldown must sleep instead of polling every10ms"
        );
        assert!(after.freelist_count > 64, "stop need not drain backlog");
    }

    #[tokio::test]
    async fn actual_role_gc_supplies_free_pages_for_physical_progress() {
        use crate::eventlog::{ConversationId, Event, EventKind, sync::network::NetworkSyncStore};
        let root = tempfile::tempdir().unwrap();
        let office = office(&root);
        let author = DeviceIdentity::generate();
        let recipient = DeviceIdentity::generate();
        let mailbox =
            crate::postoffice::opaque::RelayMailbox::federated(&author, &recipient.public())
                .unwrap();
        for seq in 1..=24 {
            let event = mailbox
                .wrap(&Event::new(
                    &author,
                    ConversationId::new([47; 32]),
                    seq,
                    vec![],
                    seq,
                    0,
                    EventKind::Message,
                    vec![1; 50_000],
                ))
                .unwrap();
            office.state.record(&event, 0, None).unwrap();
        }
        let live_recipient = DeviceIdentity::generate();
        let live =
            crate::postoffice::opaque::RelayMailbox::federated(&author, &live_recipient.public())
                .unwrap()
                .wrap(&Event::new(
                    &author,
                    ConversationId::new([48; 32]),
                    1,
                    vec![],
                    1,
                    0,
                    EventKind::Message,
                    vec![2],
                ))
                .unwrap();
        office
            .state
            .record(&live, super::super::network::unix_now(), None)
            .unwrap();
        let reads_before = office.state.body_read_counts();
        let db = office.state.profile().database();
        let before = db.physical_stats().unwrap();
        let (stop, rx) = watch::channel(false);
        let worker = office.run_maintenance(rx);
        tokio::pin!(worker);
        tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"),_=tokio::time::sleep(Duration::from_millis(550))=>{}}
        stop.send_replace(true);
        worker.await.unwrap();
        let after = db.physical_stats().unwrap();
        assert!(
            after.page_count < before.page_count,
            "logical role GC must enable physical progress: {before:?} -> {after:?}"
        );
        assert!(
            !office
                .state
                .next_work_at(super::super::network::unix_now())
                .unwrap()
                .has_work()
        );
        assert!(!office.state.digest_work_pending().unwrap());
        assert_eq!(
            office.state.body_read_counts(),
            reads_before,
            "logical and physical cleanup cannot rehydrate history bodies"
        );
        assert!(
            office
                .durable_have(&live.conversation_id, &live.id)
                .unwrap()
        );
        assert_eq!(office.state.physical_charge().unwrap().event_count, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn physical_contention_cannot_starve_other_classes_or_bypass_retry_deadline() {
        use crate::postoffice::storage::federation::{Direction, SessionOwner};
        use crate::storage::database::observations::{FORCE_BUSY, VACUUM_ATTEMPTS};
        struct ResetBusy;
        impl Drop for ResetBusy {
            fn drop(&mut self) {
                FORCE_BUSY.with(|v| v.set(false));
            }
        }
        let _reset = ResetBusy;
        let root = tempfile::tempdir().unwrap();
        let office = office(&root);
        let db = office.state.profile().database();
        db.transaction(|c| {
            c.execute_batch("CREATE TABLE disposable_pages(body BLOB)")?;
            c.execute("INSERT INTO disposable_pages VALUES(zeroblob(2097152))", [])?;
            c.execute("DELETE FROM disposable_pages", [])?;
            Ok(())
        })
        .unwrap();
        FORCE_BUSY.with(|v| v.set(true));
        VACUUM_ATTEMPTS.with(|v| v.borrow_mut().clear());
        let (stop, rx) = watch::channel(false);
        {
            let worker = office.run_maintenance(rx);
            tokio::pin!(worker);
            tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"),_=tokio::time::sleep(Duration::from_millis(1))=>{}}
            assert_eq!(VACUUM_ATTEMPTS.with(|v| v.borrow().len()), 1);
            db.transaction(|c| {
                c.execute("UPDATE relay_publication SET staging_pending=1", [])?;
                Ok(())
            })
            .unwrap();
            drop(
                SessionOwner::begin(
                    office.state.profile().clone(),
                    Direction::Inbound,
                    super::super::network::unix_now(),
                    Instant::now(),
                )
                .unwrap(),
            );
            office
                .state
                .request_preparation_at(super::super::network::unix_now())
                .unwrap();
            tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"),_=tokio::time::sleep(Duration::from_millis(350))=>{}}
            assert!(
                office
                    .state
                    .preparation_ready_at(super::super::network::unix_now())
                    .unwrap()
            );
            assert!(!office.state.profile().cleanup_pending().unwrap());
            assert_eq!(
                scalar(&office, "SELECT session_rows FROM relay_federation_runtime"),
                0
            );
            let attempts = VACUUM_ATTEMPTS.with(|v| v.borrow().clone());
            assert!(
                (2..=3).contains(&attempts.len()),
                "physical attempts: {}",
                attempts.len()
            );
            for (index, pair) in attempts.windows(2).enumerate() {
                assert!(pair[1] - pair[0] >= Duration::from_millis(100u64 << index));
            }
            let until = tokio::time::Instant::now() + Duration::from_secs(10);
            while VACUUM_ATTEMPTS.with(|v| v.borrow().len()) < 3 {
                tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"),_=tokio::time::sleep(Duration::from_millis(10))=>{}}
                assert!(tokio::time::Instant::now() < until);
            }
            let next =
                VACUUM_ATTEMPTS.with(|v| *v.borrow().last().unwrap()) + Duration::from_millis(400);
            FORCE_BUSY.with(|v| v.set(false));
            office
                .state
                .request_preparation_at(super::super::network::unix_now())
                .unwrap();
            tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"),_=tokio::time::sleep_until(next - MIN_SPACING)=>{}}
            assert_eq!(
                VACUUM_ATTEMPTS.with(|v| v.borrow().len()),
                3,
                "healthy wakes cannot bypass failed physical eligibility"
            );
            let stopped_at = tokio::time::Instant::now();
            stop.send_replace(true);
            worker.await.unwrap();
            assert_eq!(tokio::time::Instant::now(), stopped_at);
        }
        drop(office);
        assert!(ProfileStorageLease::acquire(root.path()).is_ok());
    }

    #[test]
    fn pacing_enforces_spacing_duty_and_bounded_error_backoff() {
        assert_eq!(pause_for(Duration::ZERO, 0), Duration::from_millis(10));
        assert_eq!(
            pause_for(Duration::from_millis(20), 0),
            Duration::from_millis(60)
        );
        assert_eq!(pause_for(Duration::ZERO, 1), Duration::from_millis(100));
        assert_eq!(pause_for(Duration::ZERO, 100), Duration::from_secs(5));
    }
    #[tokio::test]
    async fn demand_between_worker_creation_and_poll_is_not_lost_and_shutdown_releases_lease() {
        let root = tempfile::tempdir().unwrap();
        let office = office(&root);
        let (stop, rx) = watch::channel(false);
        {
            let worker = office.run_maintenance(rx);
            office
                .state
                .request_preparation_at(super::super::network::unix_now())
                .unwrap();
            tokio::pin!(worker);
            tokio::select! {
                result=&mut worker => panic!("unexpected worker completion: {result:?}"),
                _=tokio::time::sleep(Duration::from_millis(60)) => {}
            }
            assert!(
                office
                    .state
                    .preparation_ready_at(super::super::network::unix_now())
                    .unwrap()
            );
            stop.send_replace(true);
            worker.await.unwrap();
        }
        assert!(!office.requires_reopen());
        drop(office);
        assert!(ProfileStorageLease::acquire(root.path()).is_ok());
    }
    #[tokio::test]
    async fn idle_worker_does_not_write_database_or_start_global_preparation() {
        let root = tempfile::tempdir().unwrap();
        let office = office(&root);
        // Finish publication cleanup before observing the idle owner.
        while office.state.profile().cleanup_pending().unwrap() {
            office.state.profile().cleanup_step(true).unwrap();
        }
        crate::storage::database::observations::PASSIVE_ATTEMPTS.with(|v| v.borrow_mut().clear());
        let before = office
            .state
            .profile()
            .database()
            .read(|c| Ok(c.total_changes()))
            .unwrap();
        let (stop, rx) = watch::channel(false);
        let worker = office.run_maintenance(rx);
        tokio::pin!(worker);
        tokio::select! { r=&mut worker=>panic!("worker stopped: {r:?}"), _=tokio::time::sleep(Duration::from_millis(80))=>{} }
        stop.send_replace(true);
        worker.await.unwrap();
        assert_eq!(
            crate::storage::database::observations::PASSIVE_ATTEMPTS.with(|v| v.borrow().len()),
            0
        );
        assert_eq!(
            office
                .state
                .profile()
                .database()
                .read(|c| Ok(c.total_changes()))
                .unwrap(),
            before
        );
        assert!(
            !office
                .state
                .preparation_ready_at(super::super::network::unix_now())
                .unwrap()
        );
    }
    #[tokio::test]
    async fn commit_fault_stops_worker_and_preserves_original_database_failure() {
        let root = tempfile::tempdir().unwrap();
        let office = office(&root);
        let (_stop, rx) = watch::channel(false);
        office
            .state
            .request_preparation_at(super::super::network::unix_now())
            .unwrap();
        office.state.profile().database().read(|c| {
            c.execute_batch("CREATE TEMP TABLE worker_fault_parent(id INTEGER PRIMARY KEY); CREATE TEMP TABLE worker_fault_child(id INTEGER REFERENCES worker_fault_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TEMP TRIGGER worker_commit_fault AFTER UPDATE ON relay_digest_preparation BEGIN INSERT INTO worker_fault_child VALUES(1); END;")?; Ok(())
        }).unwrap();
        let failed = office.run_maintenance(rx).await;
        assert!(matches!(failed, Err(LogError::Database(_))));
        assert!(office.requires_reopen());
        assert!(
            office
                .state
                .preparation_ready_at(super::super::network::unix_now())
                .is_err()
        );
        drop(office);
        assert!(ProfileStorageLease::acquire(root.path()).is_ok());
    }
    #[tokio::test]
    async fn cancelled_sessions_and_warm_staging_cleanup_resume_without_global_hashing() {
        use crate::postoffice::storage::federation::{Direction, SessionOwner};
        let root = tempfile::tempdir().unwrap();
        let office = office(&root);
        office
            .state
            .profile()
            .database()
            .transaction(|c| {
                c.execute("UPDATE relay_publication SET staging_pending=1", [])?;
                Ok(())
            })
            .unwrap();
        let owner = SessionOwner::begin(
            office.state.profile().clone(),
            Direction::Inbound,
            super::super::network::unix_now(),
            Instant::now(),
        )
        .unwrap();
        drop(owner);
        let (stop, rx) = watch::channel(false);
        let worker = office.run_maintenance(rx);
        tokio::pin!(worker);
        tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"), _=tokio::time::sleep(Duration::from_millis(80))=>{}}
        stop.send_replace(true);
        worker.await.unwrap();
        assert!(!office.state.profile().cleanup_pending().unwrap());
        let rows: i64 = office
            .state
            .profile()
            .database()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT session_rows FROM relay_federation_runtime",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(rows, 0);
        assert!(!office.state.digest_work_pending().unwrap());
    }
    #[tokio::test]
    async fn d_regression_far_future_expiry_is_checked_without_instant_overflow() {
        use crate::eventlog::{ConversationId, Event, EventKind};
        let root = tempfile::tempdir().unwrap();
        let office = office(&root);
        let author = DeviceIdentity::generate();
        let recipient = DeviceIdentity::generate();
        let event =
            crate::postoffice::opaque::RelayMailbox::federated(&author, &recipient.public())
                .unwrap()
                .wrap(&Event::new(
                    &author,
                    ConversationId::new([4; 32]),
                    1,
                    vec![],
                    1,
                    0,
                    EventKind::Message,
                    vec![1],
                ))
                .unwrap();
        office
            .state
            .record(
                &event,
                u64::MAX - super::super::MAX_RELAY_AGE.as_secs(),
                None,
            )
            .unwrap();
        let (stop, rx) = watch::channel(false);
        let worker = office.run_maintenance(rx);
        tokio::pin!(worker);
        tokio::select! {r=&mut worker=>panic!("unexpected worker stop: {r:?}"),_=tokio::time::sleep(Duration::from_millis(20))=>{}}
        stop.send_replace(true);
        worker.await.unwrap();
    }
    #[tokio::test]
    async fn self_notifications_cannot_bypass_measured_slice_spacing_or_duty() {
        use crate::eventlog::{ConversationId, Event, EventKind};
        let root = tempfile::tempdir().unwrap();
        let office = office(&root);
        SLICE_TIMES.with(|v| v.borrow_mut().clear());
        let author = DeviceIdentity::generate();
        let recipient = DeviceIdentity::generate();
        let events = (0..16)
            .map(|seq| {
                crate::postoffice::opaque::RelayMailbox::federated(&author, &recipient.public())
                    .unwrap()
                    .wrap(&Event::new(
                        &author,
                        ConversationId::new([5; 32]),
                        seq,
                        vec![],
                        seq,
                        0,
                        EventKind::Message,
                        vec![1],
                    ))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let (stop, rx) = watch::channel(false);
        let producer = async {
            for event in events {
                office
                    .state
                    .record(&event, super::super::network::unix_now(), None)
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
            stop.send_replace(true);
        };
        let (worker_result, ()) = tokio::join!(office.run_maintenance(rx), producer);
        worker_result.unwrap();
        let slices = SLICE_TIMES.with(|v| v.take());
        assert!(slices.len() >= 2);
        for pair in slices.windows(2) {
            let (start, end) = pair[0];
            let next = pair[1].0;
            let pause = next.duration_since(end);
            assert!(pause >= MIN_SPACING);
            assert!(
                pause >= end.duration_since(start).saturating_mul(3),
                "measured worker duty must be <=25%"
            );
        }
    }
    #[tokio::test]
    async fn ordinary_scoped_fingerprint_demands_receive_worker_time_without_global_scan() {
        use crate::eventlog::{ConversationId, Event, EventKind, sync::network::NetworkSyncStore};
        let root = tempfile::tempdir().unwrap();
        let office = office(&root);
        let author = DeviceIdentity::generate();
        let recipient = DeviceIdentity::generate();
        let event =
            crate::postoffice::opaque::RelayMailbox::federated(&author, &recipient.public())
                .unwrap()
                .wrap(&Event::new(
                    &author,
                    ConversationId::new([6; 32]),
                    1,
                    vec![],
                    1,
                    0,
                    EventKind::Message,
                    vec![1],
                ))
                .unwrap();
        office
            .state
            .record(&event, super::super::network::unix_now(), None)
            .unwrap();
        let view = office.open_view(&event.conversation_id).unwrap();
        assert!(office.metadata(&view).unwrap().fingerprint.is_none());
        let (stop, rx) = watch::channel(false);
        let worker = office.run_maintenance(rx);
        tokio::pin!(worker);
        tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"),_=tokio::time::sleep(Duration::from_millis(80))=>{}}
        assert!(
            office
                .metadata(&office.open_view(&event.conversation_id).unwrap())
                .unwrap()
                .fingerprint
                .is_some()
        );
        stop.send_replace(true);
        worker.await.unwrap();
        let phase: i64 = office
            .state
            .profile()
            .database()
            .read(|c| {
                Ok(
                    c.query_row("SELECT phase FROM relay_digest_preparation", [], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(phase, 0);
    }
    #[test]
    fn scoped_digest_queue_saturation_never_starts_global_inventory_preparation() {
        use crate::eventlog::{ConversationId, Event, EventKind, sync::network::NetworkSyncStore};
        let root = tempfile::tempdir().unwrap();
        let office = office(&root);
        let author = DeviceIdentity::generate();
        let recipient = DeviceIdentity::generate();
        let event =
            crate::postoffice::opaque::RelayMailbox::federated(&author, &recipient.public())
                .unwrap()
                .wrap(&Event::new(
                    &author,
                    ConversationId::new([7; 32]),
                    1,
                    vec![],
                    1,
                    0,
                    EventKind::Message,
                    vec![1],
                ))
                .unwrap();
        office
            .state
            .record(&event, super::super::network::unix_now(), None)
            .unwrap();
        office
            .state
            .profile()
            .database()
            .transaction(|c| {
                for n in 0u32..1024 {
                    let mut key = [0u8; 32];
                    key[..4].copy_from_slice(&n.to_le_bytes());
                    c.execute(
                        "INSERT INTO relay_digest_demands VALUES(?1,?2)",
                        rusqlite::params![key.as_slice(), n + 1],
                    )?;
                }
                c.execute(
                    "UPDATE relay_digest_preparation SET queued=1024,next_ticket=1025",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let view = office.open_view(&event.conversation_id).unwrap();
        assert!(office.metadata(&view).unwrap().fingerprint.is_none());
        let phase: i64 = office
            .state
            .profile()
            .database()
            .read(|c| {
                Ok(
                    c.query_row("SELECT phase FROM relay_digest_preparation", [], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(phase, 0);
        let mut hashes = DigestWorker::default();
        hashes
            .step_at(&office.state, super::super::network::unix_now())
            .unwrap();
        // A distinct probe retries only its own key after one bounded stale
        // demand is removed. The queue remains capped and no scan starts.
        assert!(office.metadata(&view).unwrap().fingerprint.is_none());
        for _ in 0..1025 {
            hashes
                .step_at(&office.state, super::super::network::unix_now())
                .unwrap();
            if office
                .metadata(&office.open_view(&event.conversation_id).unwrap())
                .unwrap()
                .fingerprint
                .is_some()
            {
                break;
            }
        }
        assert_eq!(
            office
                .metadata(&office.open_view(&event.conversation_id).unwrap())
                .unwrap()
                .fingerprint,
            Some(crate::eventlog::sync::fingerprint(vec![event.id]))
        );
        let phase: i64 = office
            .state
            .profile()
            .database()
            .read(|c| {
                Ok(
                    c.query_row("SELECT phase FROM relay_digest_preparation", [], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(phase, 0);
    }
    fn archive_fixture(
        root: &tempfile::TempDir,
        count: u64,
    ) -> (
        IndexedPostOffice,
        crate::identity::device::PublicIdentity,
        crate::eventlog::Event,
        Vec<std::path::PathBuf>,
    ) {
        use crate::storage::profile::publication::tests::CUT;
        use crate::{
            eventlog::{ConversationId, Event, EventKind},
            storage::record_log::EncryptedRecordLog,
        };
        let identity = DeviceIdentity::generate();
        let public = identity.public();
        let (ed, x) = identity.secret_bytes();
        let author = DeviceIdentity::generate();
        let recipient = DeviceIdentity::generate();
        let mailbox =
            crate::postoffice::opaque::RelayMailbox::federated(&author, &recipient.public())
                .unwrap();
        let path = root.path().join("relay.log");
        let (mut writer, _) = EncryptedRecordLog::<Event>::open(&path, "pw", b"MTLOG1").unwrap();
        let mut first = None;
        for seq in 1..=count {
            let event = mailbox
                .wrap(&Event::new(
                    &author,
                    ConversationId::new([11; 32]),
                    seq,
                    vec![],
                    seq,
                    0,
                    EventKind::Message,
                    vec![1],
                ))
                .unwrap();
            writer.append_durable(&event).unwrap();
            first.get_or_insert(event);
        }
        drop(writer);
        CUT.with(|v| v.replace(Some("published")));
        assert!(
            IndexedPostOffice::open_with_lease(
                ProfileStorageLease::acquire(root.path()).unwrap(),
                &path,
                "pw",
                DeviceIdentity::from_secret_bytes(ed, x)
            )
            .is_err()
        );
        CUT.with(|v| v.replace(Some("checkpoint")));
        let office = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(root.path()).unwrap(),
            &path,
            "pw",
            identity,
        )
        .unwrap();
        assert!(office.state.profile().cleanup_pending().unwrap());
        let directory = std::fs::read_dir(root.path())
            .unwrap()
            .map(|v| v.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".relay-publication-")
            })
            .unwrap();
        let owned = std::fs::read_dir(&directory)
            .unwrap()
            .map(|v| v.unwrap().path())
            .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("raw" | "work")))
            .collect::<Vec<_>>();
        assert!(!owned.is_empty());
        (office, public, first.unwrap(), owned)
    }
    fn block_archive_checkpoint(
        office: &IndexedPostOffice,
        public: &crate::identity::device::PublicIdentity,
    ) -> crate::storage::database::EncryptedDatabase {
        let profile = office.state.profile();
        let mut binding = b"DeviceProfileV1".to_vec();
        binding.extend_from_slice(&public.ed25519_pub);
        binding.extend_from_slice(&public.x25519_pub);
        // Test-only second connection holds a real WAL reader snapshot. The
        // production runtime still owns exactly one profile connection.
        let reader = crate::storage::database::EncryptedDatabase::open(
            &profile.root().join("profile.db"),
            "pw",
            &binding,
        )
        .unwrap();
        reader
            .read(|c| {
                c.execute_batch("BEGIN")?;
                let _: i64 = c.query_row("SELECT count(*) FROM relay_sources", [], |r| r.get(0))?;
                Ok(())
            })
            .unwrap();
        profile
            .database()
            .read(|c| {
                c.busy_timeout(Duration::ZERO)?;
                Ok(())
            })
            .unwrap();
        profile
            .database()
            .transaction(|c| {
                c.execute(
                    "UPDATE relay_sources SET cleanup_error=NULL WHERE cleanup_pending=1",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        profile.database().read(|c|{c.execute_batch("CREATE TEMP TABLE archive_diagnostic_writes(value INTEGER);CREATE TEMP TRIGGER observe_archive_diagnostic AFTER UPDATE OF cleanup_error ON relay_sources BEGIN INSERT INTO archive_diagnostic_writes VALUES(1); END;")?;Ok(())}).unwrap();
        reader
    }
    fn scalar(office: &IndexedPostOffice, sql: &str) -> i64 {
        office
            .state
            .profile()
            .database()
            .read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
            .unwrap()
    }
    #[tokio::test(start_paused = true)]
    async fn persistent_archive_failure_backs_off_without_starving_staging_sessions_or_digests() {
        use crate::{
            eventlog::sync::network::NetworkSyncStore,
            postoffice::{
                FederatedMailbox,
                storage::federation::{Direction, SessionOwner},
            },
            storage::profile::publication::tests::ARCHIVE_ATTEMPTS,
        };
        let root = tempfile::tempdir().unwrap();
        let (office, public, event, owned) = archive_fixture(&root, 400);
        let barriers = [
            root.path().join("relay.log"),
            root.path().join("relay.retention"),
        ]
        .map(|p| {
            let bytes = std::fs::read(&p).unwrap();
            (p, bytes)
        });
        let foreign = owned[0].parent().unwrap().join("foreign.keep");
        std::fs::write(&foreign, b"unowned").unwrap();
        let reader = block_archive_checkpoint(&office, &public);
        assert!(office.state.profile().database().checkpoint().is_err());
        assert!(!office.requires_reopen());
        ARCHIVE_ATTEMPTS.with(|v| v.borrow_mut().clear());
        let staged_before = scalar(&office, "SELECT count(*) FROM relay_import_events");
        assert!(staged_before > 128);
        let (stop, rx) = watch::channel(false);
        {
            let worker = office.run_maintenance(rx);
            tokio::pin!(worker);
            tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"),_=tokio::time::sleep(Duration::from_millis(1))=>{}}
            let left = scalar(&office, "SELECT count(*) FROM relay_import_events");
            assert!(
                left > 0 && staged_before - left <= 128,
                "staging work must remain bounded"
            );
            assert_eq!(ARCHIVE_ATTEMPTS.with(|v| v.borrow().len()), 1);
            let owner = SessionOwner::begin(
                office.state.profile().clone(),
                Direction::Inbound,
                super::super::network::unix_now(),
                Instant::now(),
            )
            .unwrap();
            let entries = (0u32..1000)
                .map(|n| {
                    let mut key = [0u8; 32];
                    key[..4].copy_from_slice(b"MTF3");
                    key[4..8].copy_from_slice(&n.to_be_bytes());
                    FederatedMailbox {
                        id: crate::eventlog::ConversationId::new(key),
                        digest: [0; 32],
                        first_seen_secs: super::super::network::unix_now(),
                    }
                })
                .collect::<Vec<_>>();
            owner
                .lease()
                .receive_page(&entries[..512], false, super::super::network::unix_now())
                .unwrap();
            owner
                .lease()
                .receive_page(&entries[512..], true, super::super::network::unix_now())
                .unwrap();
            drop(owner);
            let view = office.open_view(&event.conversation_id).unwrap();
            assert!(office.metadata(&view).unwrap().fingerprint.is_none());
            drop(view);
            tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"),_=tokio::time::sleep(Duration::from_millis(349))=>{}}
            let attempts = ARCHIVE_ATTEMPTS.with(|v| v.borrow().clone());
            assert!(
                (2..=3).contains(&attempts.len()),
                "persistent archive failure must back off, observed {} real checkpoint attempts",
                attempts.len()
            );
            for (index, pair) in attempts.windows(2).enumerate() {
                assert!(
                    pair[1] - pair[0] >= Duration::from_millis(100u64 << index),
                    "archive backoff must grow independently of healthy wakes"
                );
            }
            assert_eq!(
                scalar(&office, "SELECT count(*) FROM archive_diagnostic_writes"),
                1,
                "identical deferred diagnostics must not be rewritten"
            );
            assert_eq!(
                scalar(&office, "SELECT count(*) FROM relay_import_events"),
                0
            );
            assert_eq!(
                scalar(&office, "SELECT staging_pending FROM relay_publication"),
                0
            );
            assert_eq!(
                scalar(&office, "SELECT session_rows FROM relay_federation_runtime"),
                0
            );
            assert_eq!(
                scalar(&office, "SELECT staged_rows FROM relay_federation_runtime"),
                0
            );
            let view = office.open_view(&event.conversation_id).unwrap();
            assert!(office.metadata(&view).unwrap().fingerprint.is_some());
            drop(view);
            assert!(owned.iter().all(|p| p.exists()));
            // Observe a fresh failed attempt before recovery. Measured duty can
            // delay the third attempt beyond the initial 350 ms observation.
            // Anchor the no-bypass proof to actual eligibility, not wall timing.
            let observation_limit = tokio::time::Instant::now() + Duration::from_secs(10);
            while ARCHIVE_ATTEMPTS.with(|v| v.borrow().len()) < 3 {
                tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"),_=tokio::time::sleep(Duration::from_millis(10))=>{}}
                assert!(tokio::time::Instant::now() < observation_limit);
            }
            let attempts = ARCHIVE_ATTEMPTS.with(|v| v.borrow().clone());
            assert_eq!(attempts.len(), 3);
            for (index, pair) in attempts.windows(2).enumerate() {
                assert!(pair[1] - pair[0] >= Duration::from_millis(100u64 << index));
            }
            // The independent contract requires 400 ms after failure three.
            let next_eligible = *attempts.last().unwrap() + Duration::from_millis(400);
            assert!(next_eligible > tokio::time::Instant::now() + MIN_SPACING);
            reader
                .read(|c| {
                    c.execute_batch("ROLLBACK")?;
                    Ok(())
                })
                .unwrap();
            drop(reader);
            let before = ARCHIVE_ATTEMPTS.with(|v| v.borrow().len());
            tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"),_=tokio::time::sleep_until(next_eligible - MIN_SPACING)=>{}}
            assert_eq!(
                ARCHIVE_ATTEMPTS.with(|v| v.borrow().len()),
                before,
                "recovery must not bypass the outstanding archive deadline"
            );
            tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"),_=tokio::time::sleep_until(next_eligible + Duration::from_secs(1))=>{}}
            assert!(!office.state.profile().cleanup_pending().unwrap());
            assert!(owned.iter().all(|p| !p.exists()));
            assert_eq!(std::fs::read(&foreign).unwrap(), b"unowned");
            for (path, bytes) in barriers {
                assert_eq!(std::fs::read(path).unwrap(), bytes);
            }
            assert!(
                office
                    .durable_have(&event.conversation_id, &event.id)
                    .unwrap()
            );
            let stopped_at = tokio::time::Instant::now();
            stop.send_replace(true);
            worker.await.unwrap();
            assert_eq!(tokio::time::Instant::now(), stopped_at);
        }
        drop(office);
        assert!(ProfileStorageLease::acquire(root.path()).is_ok());
    }
    #[tokio::test(start_paused = true)]
    async fn stopping_with_archive_backoff_pending_joins_without_waiting_for_retry() {
        use crate::storage::profile::publication::tests::ARCHIVE_ATTEMPTS;
        let root = tempfile::tempdir().unwrap();
        let (office, public, _event, owned) = archive_fixture(&root, 1);
        let reader = block_archive_checkpoint(&office, &public);
        ARCHIVE_ATTEMPTS.with(|v| v.borrow_mut().clear());
        let (stop, rx) = watch::channel(false);
        {
            let worker = office.run_maintenance(rx);
            tokio::pin!(worker);
            tokio::select! {r=&mut worker=>panic!("worker stopped: {r:?}"),_=tokio::time::sleep(Duration::from_millis(1))=>{}}
            let stopped_at = tokio::time::Instant::now();
            stop.send_replace(true);
            worker.await.unwrap();
            assert_eq!(tokio::time::Instant::now(), stopped_at);
        }
        assert_eq!(ARCHIVE_ATTEMPTS.with(|v| v.borrow().len()), 1);
        assert!(owned.iter().all(|p| p.exists()));
        assert!(office.state.profile().cleanup_pending().unwrap());
        reader
            .read(|c| {
                c.execute_batch("ROLLBACK")?;
                Ok(())
            })
            .unwrap();
        drop(reader);
        drop(office);
        assert!(ProfileStorageLease::acquire(root.path()).is_ok());
    }
}
