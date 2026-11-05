//! Opt-in, bounded progress evidence for the existing maintenance owner.
//! No event bodies, account identities or message identifiers are logged.

use super::Node;
use crate::eventlog::LogError;
use crate::eventlog::repository::order::{CausalOrder, OrderWorkCursor};
use crate::eventlog::{ConversationId, EventId};
use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const INTERVAL: Duration = Duration::from_secs(30);
const ROWS: usize = 64;

const RECENT_IDS: usize = 4096;

#[derive(Default)]
struct ChurnObservation {
    counts: [u64; 32],
    recent: HashSet<(ConversationId, EventId)>,
    fifo: VecDeque<(ConversationId, EventId)>,
    repeats: u64,
}

impl ChurnObservation {
    fn record(&mut self, class: usize, outer: Option<(ConversationId, EventId)>) {
        if let Some(count) = self.counts.get_mut(class) {
            *count = count.saturating_add(1);
        }
        if let Some(key) = outer {
            if self.recent.contains(&key) {
                self.repeats = self.repeats.saturating_add(1);
            } else {
                if self.fifo.len() == RECENT_IDS
                    && let Some(old) = self.fifo.pop_front()
                {
                    self.recent.remove(&old);
                }
                self.recent.insert(key);
                self.fifo.push_back(key);
            }
        }
    }
}

#[cfg(test)]
fn record_churn(
    slot: Option<&Mutex<ChurnObservation>>,
    class: usize,
    outer: Option<(ConversationId, EventId)>,
) {
    if let Some(slot) = slot
        && let Ok(mut observer) = slot.try_lock()
    {
        observer.record(class, outer);
    }
}

pub(super) struct OrderWorkObserver {
    started: Instant,
    last: Option<Instant>,
    cursor: Option<OrderWorkCursor>,
    header_cursor: Option<i64>,
    churn: ChurnObservation,
}

impl OrderWorkObserver {
    #[cfg(test)]
    pub(super) fn for_test() -> Mutex<Self> {
        Mutex::new(Self {
            started: Instant::now(),
            last: None,
            cursor: None,
            header_cursor: None,
            churn: ChurnObservation::default(),
        })
    }
    pub(super) fn from_env() -> Option<Mutex<Self>> {
        (std::env::var_os("MESH_TALK_TRACE_WORK").as_deref() == Some(std::ffi::OsStr::new("1")))
            .then(|| {
                Mutex::new(Self {
                    started: Instant::now(),
                    last: None,
                    cursor: None,
                    header_cursor: None,
                    churn: ChurnObservation::default(),
                })
            })
    }
}

// Static slots only; the output order is documented beside the summary.
pub(in crate::node) mod reason {
    pub const OUTER: usize = 0;
    pub const OPEN_FAILED: usize = 1;
    pub const IDENTITY_HANDLED: usize = 2;
    pub const IDENTITY_DEFERRED: usize = 3;
    pub const CONSENT_HANDLED: usize = 4;
    pub const CONSENT_DEFERRED: usize = 5;
    pub const SEMANTIC_PROFILE: usize = 6;
    pub const RETAINED_DUPLICATE: usize = 7;
    pub const ADMITTED_RETAINED: usize = 8;
    pub const ADMITTED_NONRETAINED: usize = 9;
    pub const ADMISSION_FAILED: usize = 10;
    pub const STAGED: usize = 11;
    pub const RETAINED_HIT: usize = 12;
    pub const SESSION_HIT: usize = 13;
    pub const GET: usize = 14;
    pub const GET_RETRY: usize = 15;
    pub const RECEIPT_HOT: usize = 16;
    pub const RECEIPT_DURABLE: usize = 17;
    pub const RECEIPT_REJECTED: usize = 18;
    pub const RECEIPT_MISSING: usize = 19;
    pub const VIEW_SKIP: usize = 20;
    pub const VIEW_ABSENT: usize = 21;
    pub const VIEW_STALE: usize = 22;
    pub const STAMP_UNKNOWN: usize = 23;
    pub const STAMP_CHANGED: usize = 24;
    pub const VIEW_RESUMED: usize = 25;
    pub const VIEW_NEW: usize = 26;
    pub const UPLOAD_RESET: usize = 27;
}

impl Node {
    pub(in crate::node) fn observe_mailbox_work(
        &self,
        class: usize,
        outer: Option<(ConversationId, EventId)>,
    ) {
        let Some(slot) = &self.order_work_observation else {
            return;
        };
        if let Ok(mut observer) = slot.try_lock() {
            observer.churn.record(class, outer);
        }
    }

    /// Called only by the admitted blocking maintenance owner. Disabled nodes
    /// perform no observation query; an active observer retains one cursor.
    pub(in crate::node) fn observe_order_work(&self) {
        let Some(slot) = &self.order_work_observation else {
            return;
        };
        let Ok(mut observer) = slot.try_lock() else {
            return;
        };
        let now = Instant::now();
        if observer
            .last
            .is_some_and(|last| now.duration_since(last) < INTERVAL)
        {
            return;
        }
        observer.last = Some(now);
        let cursor = observer.cursor.clone();
        let header_cursor = observer.header_cursor;
        let age_ms = observer.started.elapsed().as_millis();
        let counts = observer.churn.counts;
        let repeats = observer.churn.repeats;
        let recent = observer.churn.recent.len();
        drop(observer);
        let page = self
            .log
            .try_lock()
            .map_err(|_| LogError::CorruptFile("order observation owner unavailable".into()))
            .and_then(|log| {
                let repository = log.repository()?;
                let source = repository.source_revision()?;
                let destructive = repository.destructive_policy_revision()?;
                let repair = repository.receive_repair_revision()?;
                let profile = repository.observe_profile_work()?;
                let page = repository.observe_order_work(cursor.as_ref(), ROWS)?;
                Ok((
                    page,
                    source,
                    destructive,
                    repair,
                    profile,
                    repository.observe_committed_kinds(header_cursor)?,
                ))
            });
        match page {
            Ok((page, source, destructive, repair, profile, headers)) => {
                if let Some(job) = profile.job {
                    log::info!(target: "mesh_talk_core::node::node",
                        "profile_work source={} selected={} complete={} candidates={} candidate_bytes={} cursor={} requests_pending={}",
                        profile.source_revision, job.revision, job.complete, job.candidates,
                        job.candidate_bytes, hex::encode(job.cursor_tag), profile.requests_pending);
                } else {
                    log::info!(target: "mesh_talk_core::node::node",
                        "profile_work source={} job=absent requests_pending={}",
                        profile.source_revision, profile.requests_pending);
                }
                log::info!(
                    target: "mesh_talk_core::node::node",
                    "order_work_page age_ms={} rows={} more={} source={} destructive={} receive_repair={}",
                    age_ms,
                    page.items.len(),
                    page.next.is_some(),
                    source,
                    destructive,
                    repair
                );
                for row in page.items {
                    let kind = match row.order {
                        CausalOrder::Logical => "logical",
                        CausalOrder::WallClock => "wall",
                    };
                    log::info!(
                    target: "mesh_talk_core::node::node",
                        "order_work scope={} job={} kind={} phase={} revision={} target={} base={} cutoff={} seeded={} rank={} seed_after={} child_after={} tail_after={} pop={}",
                        hex::encode(row.scope_tag),
                        hex::encode(row.job),
                        kind,
                        row.phase,
                        row.revision,
                        row.target_count,
                        row.base_count,
                        row.cutoff,
                        row.seeded,
                        row.next_rank,
                        row.seed_after,
                        row.child_after,
                        row.tail_after,
                        row.has_pop
                    );
                }
                if let Ok(mut observer) = slot.try_lock()
                    && observer.last == Some(now)
                {
                    observer.cursor = page.next;
                    observer.header_cursor = Some(headers.0);
                }
                let cache_usage = self
                    .opaque_receive_cache
                    .try_lock()
                    .ok()
                    .map(|cache| (cache.observation_readiness(), cache.observe_usage().ok()));
                let hot_len = self
                    .opaque_upload_cache
                    .try_lock()
                    .ok()
                    .map(|cache| cache.observed_len());
                log::info!(target: "mesh_talk_core::node::node", "mailbox_cache usage_retained_records_bytes_session_records_checkpoints_records_bytes_wrappers_records_bytes={:?} hot_len={:?}", cache_usage, hot_len);
                log::info!(target: "mesh_talk_core::node::node",
                    "mailbox_work best_effort=true recent_window={} repeats={} counts={:?} header_kinds={:?} header_more={} header_reset={} labels=outer,open_failed,identity_handled,identity_deferred,consent_handled,consent_deferred,semantic_profile,retained_duplicate,admitted_retained,admitted_nonretained,admission_failed,staged,retained_hit,session_hit,get,get_retry,receipt_hot,receipt_durable,receipt_rejected,receipt_missing,view_skip,view_absent,view_stale,stamp_unknown,stamp_changed,view_resumed,view_new,upload_reset",
                    recent, repeats, counts, headers.1, headers.2, headers.3);
            }
            Err(error) => {
                if let Ok(mut observer) = slot.try_lock()
                    && observer.last == Some(now)
                {
                    observer.cursor = None;
                }
                log::warn!("order work observation unavailable: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn churn_observation_bounds_exact_fifo_and_keeps_cumulative_classes() {
        let mut observation = ChurnObservation::default();
        let mailbox = ConversationId::new([1; 32]);
        let first = EventId::new([0; 32]);
        observation.record(0, Some((mailbox, first)));
        observation.record(1, Some((mailbox, first)));
        assert_eq!(observation.repeats, 1);
        // A different mailbox is a different observation, not a repeat.
        observation.record(1, Some((ConversationId::new([2; 32]), first)));
        assert_eq!(observation.repeats, 1);
        for n in 1..=RECENT_IDS {
            let mut id = [0; 32];
            id[..8].copy_from_slice(&(n as u64).to_le_bytes());
            observation.record(0, Some((mailbox, EventId::new(id))));
        }
        assert_eq!(observation.recent.len(), RECENT_IDS);
        assert_eq!(observation.fifo.len(), RECENT_IDS);
        assert!(!observation.recent.contains(&(mailbox, first)));
        assert_eq!(observation.counts[0], RECENT_IDS as u64 + 1);
        assert_eq!(observation.counts[1], 2);
        observation.counts[0] = u64::MAX;
        observation.record(0, None);
        assert_eq!(observation.counts[0], u64::MAX);
    }

    #[test]
    fn churn_observation_contention_returns_before_guard_release() {
        let slot = std::sync::Arc::new(Mutex::new(ChurnObservation::default()));
        let held = slot.lock().unwrap();
        let other = slot.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            record_churn(Some(&other), 0, None);
            send.send(()).unwrap();
        });
        let returned = receive.recv_timeout(Duration::from_secs(2)).is_ok();
        drop(held);
        worker.join().unwrap();
        assert!(returned, "recording must never wait for the observer");
        assert_eq!(slot.lock().unwrap().counts[0], 0);
        record_churn(None, 0, None);
    }
}
