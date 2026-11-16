//! Opt-in, bounded progress evidence for the existing maintenance owner.
//! No event bodies, account identities or message identifiers are logged.

use super::Node;
use crate::eventlog::LogError;
use crate::eventlog::repository::order::{CausalOrder, OrderWorkCursor};
use crate::eventlog::{ConversationId, EventId};
use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// Explicitly requested diagnostics must survive release builds that compile
// general INFO macros out. Honor the configured target filter synchronously.
fn emit_info(logger: &dyn log::Log, args: std::fmt::Arguments<'_>) {
    let metadata = log::Metadata::builder()
        .level(log::Level::Info)
        .target("mesh_talk_core::node::node")
        .build();
    if logger.enabled(&metadata) {
        logger.log(&log::Record::builder().metadata(metadata).args(args).build());
    }
}

const INTERVAL: Duration = Duration::from_secs(30);
const ROWS: usize = 64;

const RECENT_IDS: usize = 4096;

struct ChurnObservation {
    counts: [u64; 32],
    queries: [u64; 28],
    receive: [u64; 34],
    recent: HashSet<(ConversationId, EventId)>,
    fifo: VecDeque<(ConversationId, EventId)>,
    repeats: u64,
}

impl Default for ChurnObservation {
    fn default() -> Self {
        Self {
            counts: [0; 32],
            queries: [0; 28],
            receive: [0; 34],
            recent: HashSet::new(),
            fifo: VecDeque::new(),
            repeats: 0,
        }
    }
}

impl ChurnObservation {
    fn record(&mut self, class: usize, outer: Option<(ConversationId, EventId)>) {
        if let Some(count) = self.counts.get_mut(class) {
            *count = count.saturating_add(1);
        } else if class < 60 {
            if let Some(count) = self.queries.get_mut(class - 32) {
                *count = count.saturating_add(1);
            }
        } else if let Some(count) = self.receive.get_mut(class - 60) {
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

const DISTRIBUTION_ORIGINS: usize = 64;

#[derive(Clone, Debug)]
struct DistributionOriginCount {
    file: &'static str,
    line: u32,
    column: u32,
    confirming: bool,
    count: u64,
}

// Location is a trusted compile-time input. Keep no private build-root prefix.
fn source_file_tail(file: &'static str) -> &'static str {
    if file.starts_with("src/") || file.starts_with("src\\") {
        return file;
    }
    if let Some(start) = file.rfind("/src/").or_else(|| file.rfind("\\src\\")) {
        return &file[start + 1..];
    }
    file.rsplit(['/', '\\']).next().unwrap_or(file)
}

pub(super) struct OrderWorkObserver {
    started: Instant,
    last: Option<Instant>,
    cursor: Option<OrderWorkCursor>,
    header_cursor: Option<i64>,
    churn: ChurnObservation,
    distribution_origins: Vec<DistributionOriginCount>,
    distribution_origin_overflow: u64,
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
            distribution_origins: Vec::new(),
            distribution_origin_overflow: 0,
        })
    }
    fn record_distribution_origin(
        &mut self,
        file: &'static str,
        line: u32,
        column: u32,
        confirming: bool,
    ) {
        let file = source_file_tail(file);
        if let Some(site) = self.distribution_origins.iter_mut().find(|site| {
            site.file == file
                && site.line == line
                && site.column == column
                && site.confirming == confirming
        }) {
            site.count = site.count.saturating_add(1);
        } else if self.distribution_origins.len() < DISTRIBUTION_ORIGINS {
            self.distribution_origins.push(DistributionOriginCount {
                file,
                line,
                column,
                confirming,
                count: 1,
            });
        } else {
            self.distribution_origin_overflow = self.distribution_origin_overflow.saturating_add(1);
        }
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
                    distribution_origins: Vec::new(),
                    distribution_origin_overflow: 0,
                })
            })
    }
}

// Static slots only; the output order is documented beside the summary.
pub(in crate::node) mod reason {
    pub const RECEIVE_DRAIN_START: usize = 60;
    pub const RECEIVE_DRAIN_SUCCESS: usize = 61;
    pub const RECEIVE_DRAIN_FAILED: usize = 62;
    pub const RECEIVE_DRAIN_EMPTY: usize = 63;
    pub const RECEIVE_NEGOTIATE_SUPPORTED: usize = 64;
    pub const RECEIVE_NEGOTIATE_LEGACY: usize = 65;
    pub const RECEIVE_NEGOTIATE_ERROR: usize = 66;
    pub const RECEIVE_SCOPE_LIMIT: usize = 67;
    pub const RECEIVE_REARM_OK: usize = 68;
    pub const RECEIVE_REARM_ERROR: usize = 69;
    pub const RECEIVE_WAIT_CHANGED: usize = 70;
    pub const RECEIVE_WAIT_EQUAL: usize = 71;
    pub const RECEIVE_WAIT_ERROR: usize = 72;
    pub const RECEIVE_DRAIN_DIRTY: usize = 73;
    pub const RECEIVE_SOURCE_WAKE: usize = 74;
    pub const RECEIVE_NETWORK_RESTART: usize = 75;
    pub const RECEIVE_POLICY_RESTART: usize = 76;
    pub const RECEIVE_TOPOLOGY_CANCEL: usize = 77;
    pub const RECEIVE_EXPLICIT_RESTART: usize = 78;
    pub const RECEIVE_WATCH_ABORT: usize = 79;
    pub const DRAIN_RECEIVE_GATE: usize = 80;
    pub const DRAIN_GROUP_LOOKUP_FAILED: usize = 81;
    pub const DRAIN_NO_PROGRESS: usize = 82;
    pub const DRAIN_TRANSPORT_FAILED: usize = 83;
    pub const DRAIN_STORE_FAILED: usize = 84;
    pub const DRAIN_PROTOCOL_FAILED: usize = 85;
    pub const DRAIN_TIMEOUT: usize = 86;
    pub const CHANNEL_SYNC_OK_ZERO: usize = 87;
    pub const CHANNEL_SYNC_OK_APPLIED: usize = 88;
    pub const CHANNEL_SYNC_NO_PROGRESS: usize = 89;
    pub const CHANNEL_SYNC_TRANSPORT_FAILED: usize = 90;
    pub const CHANNEL_SYNC_TIMEOUT: usize = 91;
    pub const CHANNEL_SYNC_STORE_FAILED: usize = 92;
    pub const CHANNEL_SYNC_PROTOCOL_FAILED: usize = 93;
    pub const QUERY_STAMP_UNKNOWN: usize = 32;
    pub const QUERY_META_HIT: usize = 33;
    pub const QUERY_META_MISS: usize = 34;
    pub const QUERY_META_EVICTED: usize = 35;
    pub const QUERY_VIEW_NEW: usize = 36;
    pub const QUERY_VIEW_ABSENT: usize = 37;
    pub const QUERY_SCOPE_MISS: usize = 38;
    pub const QUERY_STAMP_MISS: usize = 39;
    pub const QUERY_VIEW_INVALID: usize = 40;
    pub const QUERY_VIEW_REUSED: usize = 41;
    pub const QUERY_VIEW_RETAINED: usize = 42;
    pub const CONSENT_HIT: usize = 43;
    pub const CONSENT_MAIN_CHANGED: usize = 44;
    pub const CONSENT_POLICY_CHANGED: usize = 45;
    pub const CONSENT_REMOTE_CHANGED: usize = 46;
    pub const CONSENT_FLOOR_CHANGED: usize = 47;
    pub const CONSENT_EXPIRED: usize = 48;
    pub const CONSENT_ABSENT: usize = 49;
    pub const CONSENT_LEARNED: usize = 50;
    pub const CONSENT_PREPARE_MOVED: usize = 51;
    pub const FILE_WAITING: usize = 52;
    pub const FILE_PREPARING: usize = 53;
    pub const FILE_ALLOWED: usize = 54;
    pub const COMPLETION_MISSING: usize = 55;
    pub const COMPLETION_STAMP_UNKNOWN: usize = 56;
    pub const COMPLETION_LOCAL_CHANGED: usize = 57;
    pub const COMPLETION_REMOTE_CHANGED: usize = 58;
    pub const COMPLETION_REMOTE_CLEARED: usize = 59;
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
    pub const SEMANTIC_HIT: usize = 28;
    pub const PULL_ADVANCED: usize = 29;
    pub const PULL_QUIET: usize = 30;
    pub const PULL_DEFERRED: usize = 31;
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

    pub(super) fn observe_distribution_origin(
        &self,
        origin: &'static std::panic::Location<'static>,
        confirming: bool,
    ) {
        let Some(slot) = &self.order_work_observation else {
            return;
        };
        if let Ok(mut observer) = slot.try_lock() {
            observer.record_distribution_origin(
                origin.file(),
                origin.line(),
                origin.column(),
                confirming,
            );
        }
    }

    /// Called only by the admitted blocking maintenance owner. Disabled nodes
    /// perform no observation query; an active observer retains one cursor.
    pub(in crate::node) fn observe_order_work(&self) {
        self.observe_order_work_with_logger(log::logger());
    }

    fn observe_order_work_with_logger(&self, logger: &dyn log::Log) {
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
        let queries = observer.churn.queries;
        let receive = observer.churn.receive;
        let repeats = observer.churn.repeats;
        let recent = observer.churn.recent.len();
        let distribution_origins = observer.distribution_origins.clone();
        let distribution_origin_overflow = observer.distribution_origin_overflow;
        drop(observer);
        emit_info(
            logger,
            format_args!(
                "receive_work best_effort=true counts={:?} labels=drain_start,drain_success,drain_failed,drain_empty,negotiate_supported,negotiate_legacy,negotiate_error,scope_limit,rearm_ok,rearm_error,wait_changed,wait_equal,wait_error,drain_dirty,source_wake,network_restart,policy_restart,topology_cancel,explicit_restart,watch_abort,drain_receive_gate,drain_group_lookup_failed,drain_no_progress,drain_transport_failed,drain_store_failed,drain_protocol_or_resource_failed,drain_timeout,channel_sync_ok_zero,channel_sync_ok_applied,channel_sync_no_progress,channel_sync_transport_failed,channel_sync_timeout,channel_sync_store_failed,channel_sync_protocol_or_resource_failed mailbox_counts={:?} query_counts={:?} mailbox_recent={} mailbox_repeats={} distribution_origins={:?} distribution_origin_overflow={} age_ms={}",
                receive,
                counts,
                queries,
                recent,
                repeats,
                distribution_origins,
                distribution_origin_overflow,
                age_ms
            ),
        );
        if let Some(pages) = crate::storage::database::observe_page_writes() {
            emit_info(logger, format_args!("{pages}"));
        }
        let page = {
            let log = match self.log.try_lock() {
                Ok(log) => log,
                Err(std::sync::TryLockError::WouldBlock) => {
                    emit_info(
                        logger,
                        format_args!("order_work_page skipped=owner_busy age_ms={age_ms}"),
                    );
                    return;
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    log::warn!("order work observation unavailable: observation owner poisoned");
                    return;
                }
            };
            (|| -> Result<_, LogError> {
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
            })()
        };
        match page {
            Ok((page, source, destructive, repair, profile, headers)) => {
                if let Some(job) = profile.job {
                    emit_info(
                        logger,
                        format_args!(
                            "profile_work source={} selected={} complete={} candidates={} candidate_bytes={} cursor={} requests_pending={}",
                            profile.source_revision,
                            job.revision,
                            job.complete,
                            job.candidates,
                            job.candidate_bytes,
                            hex::encode(job.cursor_tag),
                            profile.requests_pending
                        ),
                    );
                } else {
                    emit_info(
                        logger,
                        format_args!(
                            "profile_work source={} job=absent requests_pending={}",
                            profile.source_revision, profile.requests_pending
                        ),
                    );
                }
                emit_info(
                    logger,
                    format_args!(
                        "order_work_page age_ms={} rows={} more={} source={} destructive={} receive_repair={}",
                        age_ms,
                        page.items.len(),
                        page.next.is_some(),
                        source,
                        destructive,
                        repair
                    ),
                );
                for row in page.items {
                    let kind = match row.order {
                        CausalOrder::Logical => "logical",
                        CausalOrder::WallClock => "wall",
                    };
                    emit_info(
                        logger,
                        format_args!(
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
                        ),
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
                    .map(|cache| (cache.observed_len(), cache.observed_control_hints()));
                emit_info(
                    logger,
                    format_args!(
                        "mailbox_cache usage_retained_records_bytes_session_records_checkpoints_records_bytes_wrappers_records_bytes={:?} hot_len={:?}",
                        cache_usage, hot_len
                    ),
                );
                emit_info(
                    logger,
                    format_args!(
                        "mailbox_work best_effort=true recent_window={} repeats={} counts={:?} header_kinds={:?} header_more={} header_reset={} labels=outer,open_failed,identity_handled,identity_deferred,consent_handled,consent_deferred,semantic_profile,retained_duplicate,admitted_retained,admitted_nonretained,admission_failed,staged,retained_hit,session_hit,get,get_retry,receipt_hot,receipt_durable,receipt_rejected,receipt_missing,view_skip,view_absent,view_stale,stamp_unknown,stamp_changed,view_resumed,view_new,upload_reset,semantic_hit,pull_advanced,pull_quiet,pull_deferred",
                        recent, repeats, counts, headers.1, headers.2, headers.3
                    ),
                );
                emit_info(
                    logger,
                    format_args!(
                        "profile_header_work count={} oldest_wall_clock={:?} newest_wall_clock={:?} processed_current_account={}",
                        headers.4.count,
                        headers.4.oldest_wall_clock,
                        headers.4.newest_wall_clock,
                        headers.4.processed_current_account
                    ),
                );
                // Static counters/revisions only, sampled by this existing
                // opt-in owner after releasing observer, SQL and log guards.
                let entries = self
                    .channel_projection_cache
                    .try_lock()
                    .ok()
                    .map(|c| c.len());
                let proofs = self
                    .privacy
                    .state
                    .try_read()
                    .ok()
                    .and_then(|p| p.as_ref().map(|p| p.proofs.revision()));
                let controls = self
                    .delivery_control_ids
                    .try_lock()
                    .ok()
                    .map(|c| c.revision());
                let profile_sql = self.storage.participant_database().map(|database| {
                    (
                        database.work_observation_enabled(),
                        database
                            .read(|connection| Ok(connection.total_changes()))
                            .ok(),
                    )
                });
                // Keep both counter families and their observation age in one
                // bounded record: file rotation can discard preceding rows.
                emit_info(
                    logger,
                    format_args!(
                        "query_work counts={:?} labels=stamp_unknown,metadata_hit,metadata_miss,metadata_evicted,view_new,view_absent,scope_miss,stamp_miss,view_invalid,view_reused,view_retained,consent_hit,consent_main_changed,consent_policy_changed,consent_remote_changed,consent_floor_changed,consent_expired,consent_absent,consent_learned,consent_prepare_moved,file_waiting,file_preparing,file_allowed,completion_missing,completion_stamp_unknown,completion_local_changed,completion_remote_changed,completion_remote_cleared entries={:?} privacy={} projection={} file_scopes={:?} proofs={:?} controls={:?} device_policy={} hot_views_and_consent_hints={:?} profile_sql_observer_and_total_changes={:?} mailbox_counts={:?} mailbox_recent={} mailbox_repeats={} age_ms={}",
                        queries,
                        entries,
                        self.privacy
                            .generation
                            .load(std::sync::atomic::Ordering::SeqCst),
                        self.privacy
                            .projection_revision
                            .load(std::sync::atomic::Ordering::SeqCst),
                        self.file_scope_revision().ok(),
                        proofs,
                        controls,
                        self.device_sync_policy_snapshot().1,
                        hot_len,
                        profile_sql,
                        counts,
                        recent,
                        repeats,
                        age_ms
                    ),
                );
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

    struct OwnerCapture(Mutex<Vec<String>>);
    impl log::Log for OwnerCapture {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &log::Record<'_>) {
            self.0.lock().unwrap().push(record.args().to_string());
        }
        fn flush(&self) {}
    }

    #[test]
    fn held_log_keeps_captured_work_counts_and_disabled_owner_emits_nothing() {
        use crate::{
            discovery::Announce,
            identity::{account::Account, device::DeviceIdentity},
            node::test_support::node_with_files,
        };
        let dir = tempfile::tempdir().unwrap();
        let proof = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "fixture",
            9,
        );
        let (mut node, _, _) = node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        std::sync::Arc::get_mut(&mut node)
            .unwrap()
            .order_work_observation = Some(OrderWorkObserver::for_test());
        node.observe_mailbox_work(reason::OUTER, None);
        node.observe_mailbox_work(reason::QUERY_META_HIT, None);
        node.observe_mailbox_work(reason::RECEIVE_DRAIN_START, None);
        let capture = OwnerCapture(Mutex::new(Vec::new()));
        let held = node.log.lock().unwrap();
        node.observe_order_work_with_logger(&capture);
        drop(held);
        let rows = capture.0.lock().unwrap().clone();
        let receive = rows
            .iter()
            .find(|row| row.starts_with("receive_work "))
            .expect("actual owner emits receive snapshot while log is held");
        assert!(
            receive.contains("mailbox_counts=[1, 0,"),
            "captured mailbox counters must survive unrelated held repository"
        );
        assert!(
            receive.contains("query_counts=[0, 1,"),
            "captured query counters must survive unrelated held repository"
        );
        assert!(receive.contains("age_ms="));
        assert!(
            rows.iter().any(|row| row.contains("skipped=owner_busy")),
            "busy diagnostic skip is explicit"
        );
        assert!(!rows.iter().any(|row| row.contains("corrupt log file")));
        let (mut disabled, _, _) = node_with_files(
            &dir.path().join("disabled"),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        std::sync::Arc::get_mut(&mut disabled)
            .unwrap()
            .order_work_observation = None;
        let capture = OwnerCapture(Mutex::new(Vec::new()));
        let held = disabled.log.lock().unwrap();
        disabled.observe_mailbox_work(reason::OUTER, None);
        disabled.observe_order_work_with_logger(&capture);
        drop(held);
        assert!(
            capture.0.lock().unwrap().is_empty(),
            "disabled observer has no query or log path"
        );
    }

    #[tokio::test]
    async fn distribution_origins_record_actual_first_poll_not_construction_and_keep_variants() {
        use crate::{
            discovery::Announce,
            identity::{account::Account, device::DeviceIdentity},
            node::test_support::node_with_files,
        };
        use futures_util::FutureExt;
        let dir = tempfile::tempdir().unwrap();
        let proof = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "fixture",
            9,
        );
        let (mut node, _, _) = node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        std::sync::Arc::get_mut(&mut node)
            .unwrap()
            .order_work_observation = None;
        let channel = node
            .create_channel("origin cancellation", Vec::new())
            .await
            .unwrap();
        // A real signed source tail needs bounded canonical preparation, so the
        // first actual poll can be cancelled before distribution reaches I/O.
        for _ in 0..256 {
            node.append_event(channel, crate::eventlog::EventKind::Message, vec![0; 8])
                .unwrap();
        }
        std::sync::Arc::get_mut(&mut node)
            .unwrap()
            .order_work_observation = Some(OrderWorkObserver::for_test());
        drop(node.distribute_channel(channel, &[]));
        drop(node.distribute_channel_confirming(channel, &[], None));
        assert!(
            node.order_work_observation
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .distribution_origins
                .is_empty()
        );
        let first = node.distribute_channel(channel, &[]).now_or_never();
        assert!(
            first.is_none(),
            "actual bounded metadata work leaves the first poll pending; dropping it cancels the same future"
        );
        let unknown = ConversationId::new([251; 32]);
        assert_eq!(
            node.distribute_channel_confirming(unknown, &[], None)
                .now_or_never(),
            Some(false)
        );
        let sites = node
            .order_work_observation
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .distribution_origins
            .clone();
        assert_eq!(
            sites.iter().map(|site| site.count).sum::<u64>(),
            2,
            "each actually polled future records exactly once"
        );
        assert!(sites.iter().any(|site| site.confirming && site.count == 1));
        assert!(sites.iter().any(|site| !site.confirming && site.count == 1));
        assert!(
            sites
                .iter()
                .all(|site| site.file.ends_with("work_observation.rs")
                    && site.line > 0
                    && site.column > 0),
            "factory captures its caller, not channels.rs or an async poll wrapper"
        );
        let capture = OwnerCapture(Mutex::new(Vec::new()));
        let held = node.log.lock().unwrap();
        node.observe_order_work_with_logger(&capture);
        drop(held);
        assert!(
            capture
                .0
                .lock()
                .unwrap()
                .iter()
                .any(|row| row.starts_with("receive_work ")
                    && row.contains("distribution_origins=[DistributionOriginCount")
                    && row.contains("work_observation.rs")),
            "static origins survive a busy supplemental repository"
        );
        std::sync::Arc::get_mut(&mut node)
            .unwrap()
            .order_work_observation = None;
        assert_eq!(
            node.distribute_channel_confirming(unknown, &[], None)
                .now_or_never(),
            Some(false)
        );
        let capture = OwnerCapture(Mutex::new(Vec::new()));
        node.observe_order_work_with_logger(&capture);
        assert!(
            capture.0.lock().unwrap().is_empty(),
            "trace-off factories retain and emit no origins"
        );
    }

    #[test]
    fn distribution_origin_bounds_saturates_and_removes_private_build_prefixes() {
        assert_eq!(
            source_file_tail(
                "/Users/private-user/build/crates/mesh-talk-core/src/node/channels.rs"
            ),
            "src/node/channels.rs"
        );
        assert_eq!(
            source_file_tail(r"C:\Users\private-user\build\src\node\channels.rs"),
            r"src\node\channels.rs"
        );
        assert_eq!(source_file_tail("channels.rs"), "channels.rs");
        let slot = OrderWorkObserver::for_test();
        let mut observer = slot.lock().unwrap();
        for line in 0..DISTRIBUTION_ORIGINS as u32 {
            observer.record_distribution_origin("src/node/fixture.rs", line, 1, false);
        }
        observer.record_distribution_origin("src/node/overflow.rs", 99, 1, false);
        assert_eq!(observer.distribution_origins.len(), DISTRIBUTION_ORIGINS);
        assert_eq!(observer.distribution_origin_overflow, 1);
        observer.distribution_origins[0].count = u64::MAX;
        observer.record_distribution_origin("src/node/fixture.rs", 0, 1, false);
        assert_eq!(observer.distribution_origins[0].count, u64::MAX);
        observer.distribution_origin_overflow = u64::MAX;
        observer.record_distribution_origin("src/node/overflow.rs", 100, 1, true);
        assert_eq!(observer.distribution_origin_overflow, u64::MAX);
    }

    #[test]
    fn explicit_work_diagnostics_honor_target_filter_without_info_macro() {
        struct Capture {
            enabled: bool,
            rows: Mutex<Vec<String>>,
        }
        impl log::Log for Capture {
            fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
                assert_eq!(metadata.level(), log::Level::Info);
                assert_eq!(metadata.target(), "mesh_talk_core::node::node");
                self.enabled
            }
            fn log(&self, record: &log::Record<'_>) {
                self.rows.lock().unwrap().push(record.args().to_string());
            }
            fn flush(&self) {}
        }
        let enabled = Capture {
            enabled: true,
            rows: Mutex::new(Vec::new()),
        };
        emit_info(&enabled, format_args!("sql-page-work {}", 7));
        assert_eq!(*enabled.rows.lock().unwrap(), ["sql-page-work 7"]);
        let disabled = Capture {
            enabled: false,
            rows: Mutex::new(Vec::new()),
        };
        emit_info(&disabled, format_args!("sql-page-work {}", 8));
        assert!(disabled.rows.lock().unwrap().is_empty());
    }

    #[test]
    fn receive_observation_has_independent_bounded_saturating_slots() {
        let mut observation = ChurnObservation::default();
        observation.record(reason::OUTER, None);
        observation.record(reason::QUERY_META_HIT, None);
        observation.record(reason::RECEIVE_DRAIN_START, None);
        observation.record(reason::RECEIVE_WATCH_ABORT, None);
        observation.record(reason::CHANNEL_SYNC_PROTOCOL_FAILED, None);
        observation.record(100, None);
        observation.record(usize::MAX, None);
        assert_eq!(observation.counts[0], 1);
        assert_eq!(observation.queries[1], 1);
        assert_eq!(observation.receive[0], 1);
        assert_eq!(observation.receive[19], 1);
        assert_eq!(observation.receive[33], 1);
        assert_eq!(observation.receive.iter().sum::<u64>(), 3);
        observation.receive[0] = u64::MAX;
        observation.record(reason::RECEIVE_DRAIN_START, None);
        assert_eq!(observation.receive[0], u64::MAX);
    }

    #[test]
    fn churn_observation_bounds_exact_fifo_and_keeps_cumulative_classes() {
        let mut observation = ChurnObservation::default();
        for class in reason::COMPLETION_MISSING..=reason::COMPLETION_REMOTE_CLEARED {
            observation.record(class, None);
        }
        assert_eq!(&observation.queries[23..], &[1; 5]);
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
        observation.record(reason::QUERY_META_HIT, None);
        observation.record(usize::MAX, None);
        assert_eq!(observation.queries[1], 1);
        assert_eq!(observation.recent.len(), RECENT_IDS);
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
