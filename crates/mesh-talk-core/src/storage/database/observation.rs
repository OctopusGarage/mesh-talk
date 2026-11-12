//! Opt-in bounded committed-write attribution. Contains static source locations only.
use std::{
    collections::BTreeMap,
    panic::Location,
    sync::Mutex,
    time::{Duration, Instant},
};
const LIMIT: usize = 64;
const INTERVAL: Duration = Duration::from_secs(30);
pub(crate) const READINESS_REASONS: usize = 16;
#[derive(Clone, Copy)]
#[repr(u8)]
pub(crate) enum ReadinessReason {
    SourceAbsent,
    CurrentAbsent,
    CurrentAccount,
    CurrentJob,
    CurrentRevision,
    CurrentRank,
    MetadataAbsent,
    MetadataAccount,
    MetadataGeneration,
    MetadataJob,
    MetadataStamp,
    MetadataReplica,
    Stale,
    Unavailable,
    ConversationAbsent,
    PredicateMoved,
}
impl ReadinessReason {
    pub(crate) fn mask(self) -> u16 {
        1 << self as u8
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Site {
    file: &'static str,
    line: u32,
}
impl Site {
    pub(super) fn caller(location: &'static Location<'static>) -> Self {
        Self {
            file: location.file(),
            line: location.line(),
        }
    }
}
#[derive(Default, Debug)]
pub(super) struct Count {
    commits: u64,
    rows: u64,
}
#[derive(Debug)]
pub(super) struct Snapshot {
    sites: BTreeMap<Site, Count>,
    overflow: Count,
    readiness: [u64; READINESS_REASONS],
}
pub(super) struct Observer {
    since: Instant,
    sites: BTreeMap<Site, Count>,
    overflow: Count,
    readiness: [u64; READINESS_REASONS],
}
impl Observer {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            since: now,
            sites: BTreeMap::new(),
            overflow: Count::default(),
            readiness: [0; READINESS_REASONS],
        }
    }
    fn record(&mut self, site: Site, rows: u64, now: Instant) -> Option<Snapshot> {
        if rows == 0 {
            return None;
        }
        let count = if self.sites.contains_key(&site) || self.sites.len() < LIMIT {
            self.sites.entry(site).or_default()
        } else {
            &mut self.overflow
        };
        count.commits = count.commits.saturating_add(1);
        count.rows = count.rows.saturating_add(rows);
        self.flush(now)
    }
    fn record_readiness(&mut self, mask: u16, now: Instant) -> Option<Snapshot> {
        for (index, count) in self.readiness.iter_mut().enumerate() {
            if mask & (1 << index) != 0 {
                *count = count.saturating_add(1);
            }
        }
        self.flush(now)
    }
    fn flush(&mut self, now: Instant) -> Option<Snapshot> {
        if now.duration_since(self.since) < INTERVAL {
            return None;
        }
        self.since = now;
        Some(Snapshot {
            sites: std::mem::take(&mut self.sites),
            overflow: std::mem::take(&mut self.overflow),
            readiness: std::mem::take(&mut self.readiness),
        })
    }
    #[cfg(test)]
    pub(super) fn snapshot_for_test(&mut self) -> Snapshot {
        Snapshot {
            sites: std::mem::take(&mut self.sites),
            overflow: std::mem::take(&mut self.overflow),
            readiness: std::mem::take(&mut self.readiness),
        }
    }
    #[cfg(test)]
    pub(super) fn readiness_for_test(&self) -> [u64; READINESS_REASONS] {
        self.readiness
    }
    #[cfg(test)]
    pub(super) fn totals(&self) -> (u64, u64) {
        self.sites
            .values()
            .chain(std::iter::once(&self.overflow))
            .fold((0, 0), |a, c| (a.0 + c.commits, a.1 + c.rows))
    }
}
impl std::fmt::Display for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sql-write-work")?;
        for (site, count) in &self.sites {
            write!(
                f,
                " [{}:{} commits={} rows={}]",
                site.file, site.line, count.commits, count.rows
            )?;
        }
        write!(
            f,
            " overflow_commits={} overflow_rows={}",
            self.overflow.commits, self.overflow.rows
        )?;
        write!(
            f,
            "\nsql-readiness-work counts={:?} labels=source_absent,current_absent,current_account,current_job,current_revision,current_rank,metadata_absent,metadata_account,metadata_generation,metadata_job,metadata_stamp,metadata_replica,stale,diagnostic_unavailable,conversation_absent,predicate_moved",
            self.readiness
        )
    }
}
pub(super) fn from_env() -> Option<Mutex<Observer>> {
    let enabled =
        std::env::var_os("MESH_TALK_TRACE_WORK").as_deref() == Some(std::ffi::OsStr::new("1"));
    #[cfg(test)]
    let enabled = TEST_ENABLED.with(|value| value.get()).unwrap_or(enabled);
    enabled.then(|| Mutex::new(Observer::new(Instant::now())))
}
#[cfg(test)]
thread_local! {
    static TEST_ENABLED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}
#[cfg(test)]
pub(crate) fn with_work_observation_for_test<T>(enabled: bool, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<bool>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_ENABLED.with(|value| value.set(self.0));
        }
    }
    let _restore = Restore(TEST_ENABLED.with(|value| value.replace(Some(enabled))));
    run()
}
pub(super) fn record(slot: &Mutex<Observer>, site: Site, rows: u64) -> Option<Snapshot> {
    slot.try_lock().ok()?.record(site, rows, Instant::now())
}
pub(super) fn record_readiness(slot: &Mutex<Observer>, mask: u16) -> Option<Snapshot> {
    slot.try_lock().ok()?.record_readiness(mask, Instant::now())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn site(line: u32) -> Site {
        Site {
            file: "static-owner.rs",
            line,
        }
    }
    #[test]
    fn readiness_masks_are_fixed_bounded_saturating_and_share_the_window() {
        let start = Instant::now();
        let mut observer = Observer::new(start);
        observer.readiness[0] = u64::MAX;
        assert!(observer.record_readiness(u16::MAX, start).is_none());
        assert_eq!(observer.readiness[0], u64::MAX);
        assert!(observer.readiness[1..].iter().all(|count| *count == 1));
        let snapshot = observer.record(site(1), 1, start + INTERVAL).unwrap();
        assert_eq!(snapshot.readiness.len(), READINESS_REASONS);
        assert_eq!(snapshot.readiness[15], 1);
        assert_eq!(observer.readiness, [0; READINESS_REASONS]);
        assert!(snapshot.to_string().contains("sql-readiness-work"));
    }
    #[test]
    fn committed_rows_and_sites_are_bounded_and_windowed() {
        let start = Instant::now();
        let mut o = Observer::new(start);
        for n in 0..LIMIT + 3 {
            assert!(o.record(site(n as u32), 2, start).is_none());
        }
        assert_eq!(o.sites.len(), LIMIT);
        assert_eq!(o.totals(), ((LIMIT + 3) as u64, 2 * (LIMIT + 3) as u64));
        let result = o.record(site(0), 5, start + INTERVAL).unwrap();
        assert_eq!(result.sites.len(), LIMIT);
        assert_eq!(result.overflow.commits, 3);
        assert_eq!(result.overflow.rows, 6);
        assert_eq!(result.sites[&site(0)].rows, 7);
        assert_eq!(o.totals(), (0, 0));
    }
    #[test]
    fn contention_never_waits_for_the_observer() {
        let slot = std::sync::Arc::new(Mutex::new(Observer::new(Instant::now())));
        let guard = slot.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = slot.clone();
        let task = std::thread::spawn(move || {
            tx.send(record(&worker, site(1), 2).is_none()).unwrap();
        });
        let result = rx.recv_timeout(Duration::from_secs(1));
        drop(guard);
        task.join().unwrap();
        assert_eq!(result, Ok(true));
    }
}
