//! Bounded discovery over a host-owned persistent snapshot.
//!
//! Preparation borrows the original checkpoint and releases no advanced state.
//! The host fetches/verifies bodies, then commits them under current authorization.
//! Only a successful atomic commit callback releases the next checkpoint. This
//! operation does not create tasks, own a database, or implement causal repair.
use crate::{
    EventId,
    range::{Checkpoint, Difference, Prefix, Progress, RangeRead, RemoteError, SummaryFrame},
};

/// Captured source facts; tokens never grant authority by themselves.
#[derive(Clone, Copy, Debug)]
pub struct ReaderSource {
    pub scope: [u8; 32],
    pub authority: [u8; 32],
    pub view: [u8; 32],
    pub revision: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct ReaderLimits {
    pub queries: usize,
    pub records: usize,
}
impl Default for ReaderLimits {
    fn default() -> Self {
        Self {
            queries: 8,
            records: 200,
        }
    }
}
#[derive(Debug)]
pub enum PrepareError<Read, Query> {
    InvalidLimits,
    Read(Read),
    Query(Query),
    Invalidated,
    InvalidFrame,
    /// No leaf batch fits. Increase the record allowance or choose another
    /// protocol; never acknowledge the batch or claim an empty completion.
    RecordAllowance {
        required: usize,
        allowance: usize,
    },
}
/// Bounded discovery facts, not a delivery receipt or a durable cursor.
#[derive(Debug)]
pub struct PreparedReaderTurn {
    next: Checkpoint,
    difference: Difference,
}
impl PreparedReaderTurn {
    pub fn missing(&self) -> &[EventId] {
        &self.difference.missing
    }
    pub fn progress(&self) -> &Progress {
        &self.difference.progress
    }
    pub fn examined(&self) -> usize {
        self.difference.examined
    }
    /// The host must fetch/authenticate the selected bodies and requalify the
    /// captured scope, source view and authority inside its atomic transaction.
    /// Commit failure must leave bodies and receive state unchanged. Only then
    /// may the caller replace its checkpoint with the returned checkpoint.
    ///
    /// Continuation requires the same pinned local reader. If that snapshot is
    /// retired or changed, restart discovery from the committed live index.
    /// Dropping this value, including on body-fetch cancellation, loses no
    /// durable progress. The callback is synchronous to avoid a cancellation
    /// gap between durable acceptance and releasing continuation state.
    pub fn commit<C>(
        self,
        commit: impl FnOnce(&[EventId]) -> Result<(), C>,
    ) -> Result<(Checkpoint, Difference), C> {
        commit(&self.difference.missing)?;
        Ok((self.next, self.difference))
    }
}
/// Prepare at most `queries` remote frames and `records` missing IDs.
/// `query` must bound transport allocation/decoding. `authorize` requalifies
/// the authenticated peer, snapshot and current policy before/after exchanges;
/// the commit callback must check them again in the write transaction.
///
/// Errors and cancellation leave the input checkpoint unchanged. A leaf batch
/// that exceeds the remaining allowance is retried on the next turn; a batch
/// larger than the whole allowance is an explicit error. Local read failure is
/// distinct from remote rejection or snapshot invalidation.
pub async fn prepare<R: RangeRead + ?Sized, Q>(
    checkpoint: &Checkpoint,
    local: &R,
    source: ReaderSource,
    limits: ReaderLimits,
    mut authorize: impl FnMut() -> bool,
    mut query: impl AsyncFnMut(Prefix) -> Result<SummaryFrame, Q>,
) -> Result<PreparedReaderTurn, PrepareError<R::Error, Q>> {
    if limits.queries == 0 || limits.records == 0 {
        return Err(PrepareError::InvalidLimits);
    }
    if !authorize() {
        return Err(PrepareError::Invalidated);
    }
    if !checkpoint
        .valid_reader_binding(
            local,
            source.scope,
            source.view,
            source.authority,
            source.revision,
        )
        .map_err(PrepareError::Read)?
    {
        return Err(PrepareError::Invalidated);
    }
    let mut next = checkpoint.clone();
    let mut missing = Vec::new();
    let mut examined = 0;
    while examined < limits.queries && missing.len() < limits.records {
        let Some(prefix) = next.next_query() else {
            break;
        };
        if !authorize() {
            return Err(PrepareError::Invalidated);
        }
        let frame = query(prefix).await.map_err(PrepareError::Query)?;
        examined += 1;
        if !authorize() {
            return Err(PrepareError::Invalidated);
        }
        let prior = next.clone();
        let ids = next
            .accept_reader(
                local,
                source.scope,
                source.view,
                source.authority,
                source.revision,
                frame,
            )
            .map_err(PrepareError::Read)?
            .map_err(|error| match error {
                RemoteError::Invalidated => PrepareError::Invalidated,
                _ => PrepareError::InvalidFrame,
            })?;
        if ids.len() > limits.records - missing.len() {
            if missing.is_empty() {
                return Err(PrepareError::RecordAllowance {
                    required: ids.len(),
                    allowance: limits.records,
                });
            }
            next = prior;
            break;
        }
        missing.extend(ids);
    }
    if !authorize() {
        return Err(PrepareError::Invalidated);
    }
    let progress = if next.has_pending() {
        Progress::More
    } else {
        Progress::Complete
    };
    Ok(PreparedReaderTurn {
        next,
        difference: Difference {
            progress,
            missing,
            examined,
        },
    })
}

impl<R: std::fmt::Display, Q: std::fmt::Display> std::fmt::Display for PrepareError<R, Q> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidLimits => f.write_str("invalid reader limits"),
            Self::Read(e) => write!(f, "local read failed: {e}"),
            Self::Query(e) => write!(f, "remote query failed: {e}"),
            Self::Invalidated => {
                f.write_str("reader snapshot or authorization invalidated; restart discovery")
            }
            Self::InvalidFrame => f.write_str("invalid remote summary frame"),
            Self::RecordAllowance {
                required,
                allowance,
            } => write!(
                f,
                "leaf batch requires {required} records but allowance is {allowance}; increase the record allowance"
            ),
        }
    }
}
impl<R: std::error::Error + 'static, Q: std::error::Error + 'static> std::error::Error
    for PrepareError<R, Q>
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(e) => Some(e),
            Self::Query(e) => Some(e),
            _ => None,
        }
    }
}
