//! Opt-in, bounded progress evidence for the existing maintenance owner.
//! No event bodies, account identities or message identifiers are logged.

use super::Node;
use crate::eventlog::LogError;
use crate::eventlog::repository::order::{CausalOrder, OrderWorkCursor};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const INTERVAL: Duration = Duration::from_secs(30);
const ROWS: usize = 64;

pub(super) struct OrderWorkObserver {
    started: Instant,
    last: Option<Instant>,
    cursor: Option<OrderWorkCursor>,
}

impl OrderWorkObserver {
    pub(super) fn from_env() -> Option<Mutex<Self>> {
        (std::env::var_os("MESH_TALK_TRACE_WORK").as_deref() == Some(std::ffi::OsStr::new("1")))
            .then(|| {
                Mutex::new(Self {
                    started: Instant::now(),
                    last: None,
                    cursor: None,
                })
            })
    }
}

impl Node {
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
        let page = self
            .log
            .lock()
            .map_err(|_| LogError::CorruptFile("order observation owner unavailable".into()))
            .and_then(|log| {
                let repository = log.repository()?;
                let source = repository.source_revision()?;
                let destructive = repository.destructive_policy_revision()?;
                let repair = repository.receive_repair_revision()?;
                let page = repository.observe_order_work(observer.cursor.as_ref(), ROWS)?;
                Ok((page, source, destructive, repair))
            });
        match page {
            Ok((page, source, destructive, repair)) => {
                log::info!(
                    target: "mesh_talk_core::node::node",
                    "order_work_page age_ms={} rows={} more={} source={} destructive={} receive_repair={}",
                    observer.started.elapsed().as_millis(),
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
                observer.cursor = page.next;
            }
            Err(error) => {
                observer.cursor = None;
                log::warn!("order work observation unavailable: {error}");
            }
        }
    }
}
