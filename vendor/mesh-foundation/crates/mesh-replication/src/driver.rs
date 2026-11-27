//! One-direction bounded synchronization over an authorized indexed view.
//! `complete` means all discovered remote IDs were durably accepted, not equality
//! or delivery to another replica. Hosts schedule turns and own durable commits.
use crate::{
    CompleteInventory, EventId,
    pull::{self, PullLimits, PullTurnLimits, Record},
    range::{Checkpoint, Prefix, Progress, RangeIndex, RemoteError, SummaryFrame},
};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug)]
pub struct DriverLimits {
    pub pull: PullTurnLimits,
    pub batch: PullLimits,
    pub nodes: usize,
    pub pages: usize,
    /// Local indexed summaries examined; does not assert remote SQL scan cost.
    pub scanned: usize,
}
impl Default for DriverLimits {
    fn default() -> Self {
        Self {
            pull: PullTurnLimits::default(),
            batch: PullLimits {
                records_per_batch: 32,
                bytes_per_batch: 60 * 1024,
            },
            nodes: 2000,
            pages: 8,
            scanned: 2000,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DriverProgress {
    pub committed: usize,
    pub bytes: usize,
    pub requests: usize,
    pub nodes: usize,
    pub pages: usize,
    pub scanned: usize,
    pub complete: bool,
}
#[derive(Debug)]
pub enum FetchError<F> {
    Exchange(F),
    Invalidated,
}
#[derive(Debug)]
pub enum DriverError<Q, F, C> {
    InvalidLimits,
    Invalidated {
        progress: DriverProgress,
    },
    Query {
        progress: DriverProgress,
        error: RemoteError<Q>,
    },
    Pull {
        progress: DriverProgress,
        error: pull::PullError<FetchError<F>, C>,
    },
}
/// Retained, bounded work for one source snapshot and authorization epoch.
/// Dropping this object loses optimization only: restart from committed index.
pub struct SyncDriver {
    checkpoint: Checkpoint,
    pending: BTreeSet<EventId>,
    discovered: bool,
    authority: [u8; 32],
    view: [u8; 32],
    remote_revision: u64,
    local_revision: u64,
    local_root: crate::range::Summary,
}
impl SyncDriver {
    pub fn new(
        scope: [u8; 32],
        authority: [u8; 32],
        local: &RangeIndex,
        remote_revision: u64,
        view: [u8; 32],
    ) -> Self {
        Self {
            checkpoint: Checkpoint::for_remote(scope, authority, local, remote_revision),
            pending: BTreeSet::new(),
            discovered: false,
            authority,
            view,
            remote_revision,
            local_revision: local.revision(),
            local_root: local.summary(Prefix::root()),
        }
    }
    /// Reauthorization must cover the pinned scope, source snapshot and local
    /// authorization generation. Failure discards the driver; never reuse a session
    /// interrupted during a frame. The commit callback must be atomic and durable.
    pub async fn turn<Q, F, C>(
        &mut self,
        local: &mut RangeIndex,
        limits: DriverLimits,
        mut authorize: impl FnMut() -> bool,
        mut query: impl AsyncFnMut(Prefix) -> Result<SummaryFrame, Q>,
        mut fetch: impl AsyncFnMut(&[EventId], usize) -> Result<Vec<Record>, F>,
        mut commit: impl FnMut(&[Record]) -> Result<(), C>,
    ) -> Result<DriverProgress, DriverError<Q, F, C>> {
        if limits.batch.records_per_batch == 0
            || limits.batch.bytes_per_batch == 0
            || limits.pull.records == 0
            || limits.pull.bytes == 0
            || limits.pull.requests == 0
            || limits.nodes == 0
            || limits.pages == 0
            || limits.scanned == 0
        {
            return Err(DriverError::InvalidLimits);
        }
        let mut result = DriverProgress::default();
        if local.revision() != self.local_revision
            || local.summary(Prefix::root()) != self.local_root
        {
            return Err(DriverError::Invalidated { progress: result });
        }
        loop {
            if !authorize() {
                return Err(DriverError::Invalidated { progress: result });
            }
            if !self.pending.is_empty()
                && (self.pending.len()
                    >= limits
                        .batch
                        .records_per_batch
                        .min(limits.pull.records - result.committed)
                    || self.discovered
                    || result.nodes == limits.nodes
                    || result.pages == limits.pages
                    || result.scanned == limits.scanned
                    || result.requests.saturating_add(1) >= limits.pull.requests)
            {
                if result.committed == limits.pull.records
                    || result.bytes == limits.pull.bytes
                    || result.requests == limits.pull.requests
                {
                    return Ok(result);
                }
                let ids: Vec<_> = self.pending.iter().copied().collect();
                let remote = CompleteInventory::from_ids(&ids, ids.len())
                    .expect("bounded unique pending IDs");
                let empty = CompleteInventory::from_ids(&[], 0).expect("empty inventory");
                let checkpoint_valid = std::cell::Cell::new(true);
                let fetch_requests = std::cell::Cell::new(0usize);
                let pulled = pull::pull_missing_bounded(
                    &remote,
                    &empty,
                    limits.batch,
                    PullTurnLimits {
                        records: limits.pull.records - result.committed,
                        bytes: limits.pull.bytes - result.bytes,
                        requests: limits.pull.requests - result.requests,
                    },
                    async |ids, budget| {
                        fetch_requests.set(fetch_requests.get() + 1);
                        if !checkpoint_valid.get() || !authorize() {
                            return Err(FetchError::Invalidated);
                        }
                        let records = fetch(ids, budget).await.map_err(FetchError::Exchange)?;
                        if !authorize() {
                            return Err(FetchError::Invalidated);
                        }
                        Ok(records)
                    },
                    |records| {
                        // Authorization is checked again after suspended I/O, before durable commit.
                        // Capture denial without calling the host commit below.
                        commit(records)?;
                        result.committed += records.len();
                        result.bytes += records.iter().map(|r| r.body.len()).sum::<usize>();
                        let committed_ids: Vec<_> = records.iter().map(|r| r.id).collect();
                        checkpoint_valid.set(
                            self.checkpoint
                                .acknowledge_insertions(local, &committed_ids),
                        );
                        self.local_revision = local.revision();
                        self.local_root = local.summary(Prefix::root());
                        for id in committed_ids {
                            self.pending.remove(&id);
                        }
                        Ok(())
                    },
                )
                .await;
                if !checkpoint_valid.get() {
                    return Err(DriverError::Invalidated { progress: result });
                }
                result.requests += fetch_requests.get();
                let pulled = pulled.map_err(|error| DriverError::Pull {
                    progress: result,
                    error,
                })?;
                if !pulled.complete {
                    return Ok(result);
                }
            }
            if self.discovered {
                result.complete = true;
                return Ok(result);
            }
            if result.nodes == limits.nodes
                || result.pages == limits.pages
                || result.scanned == limits.scanned
                || result.requests == limits.pull.requests
            {
                return Ok(result);
            }
            let query_calls = std::cell::Cell::new(0usize);
            let difference = self
                .checkpoint
                .advance_remote(
                    local,
                    self.view,
                    self.authority,
                    self.remote_revision,
                    1,
                    &mut authorize,
                    async |prefix| {
                        query_calls.set(query_calls.get() + 1);
                        query(prefix).await
                    },
                )
                .await;
            result.requests += query_calls.get();
            result.pages += query_calls.get();
            result.nodes += query_calls.get();
            result.scanned += query_calls.get();
            let difference = difference.map_err(|error| DriverError::Query {
                progress: result,
                error,
            })?;
            if difference.progress == Progress::Invalidated {
                return Err(DriverError::Invalidated { progress: result });
            }
            self.pending.extend(difference.missing);
            self.discovered = difference.progress == Progress::Complete;
        }
    }
}

impl<F: std::fmt::Display> std::fmt::Display for FetchError<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exchange(e) => write!(f, "body exchange failed: {e}"),
            Self::Invalidated => {
                f.write_str("body snapshot or authorization invalidated; restart synchronization")
            }
        }
    }
}
impl<F: std::error::Error + 'static> std::error::Error for FetchError<F> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Exchange(e) => Some(e),
            _ => None,
        }
    }
}
impl<Q: std::fmt::Display, F: std::fmt::Display, C: std::fmt::Display> std::fmt::Display
    for DriverError<Q, F, C>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidLimits => f.write_str("invalid synchronization limits"),
            Self::Invalidated { progress } => write!(
                f,
                "synchronization invalidated after {} commits; restart synchronization",
                progress.committed
            ),
            Self::Query { progress, error } => write!(
                f,
                "synchronization query failed after {} commits: {error}",
                progress.committed
            ),
            Self::Pull { progress, error } => write!(
                f,
                "synchronization pull failed after {} commits: {error}",
                progress.committed
            ),
        }
    }
}
impl<Q: std::error::Error + 'static, F: std::error::Error + 'static, C: std::error::Error + 'static>
    std::error::Error for DriverError<Q, F, C>
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Query { error, .. } => Some(error),
            Self::Pull { error, .. } => Some(error),
            _ => None,
        }
    }
}
