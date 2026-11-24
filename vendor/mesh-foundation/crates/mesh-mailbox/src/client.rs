//! One budgeted typed operation. No wire codec, storage format or scheduling policy.
use crate::{
    model::{MailboxCursor, MailboxDeltaPage, Record, ScopeId},
    ports::MailboxTransport,
};
use mesh_replication::pull::{PullProgress, PullTurnLimits};
#[derive(Clone, Copy, Debug)]
pub struct MailboxTurnLimits {
    pub work: PullTurnLimits,
    pub pages: usize,
    pub page_records: usize,
}
impl Default for MailboxTurnLimits {
    fn default() -> Self {
        Self {
            work: PullTurnLimits::default(),
            pages: 8,
            page_records: 64,
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct MailboxProgress {
    pub cursor: Option<MailboxCursor>,
    pub work: PullProgress,
    pub pages: usize,
}
#[derive(Debug)]
pub enum MailboxTurnErrorKind<Io, Commit> {
    InvalidLimits,
    Transport(Io),
    InvalidPage,
    InvalidRecords,
    BodyLimit,
    Commit(Commit),
    CheckpointInvalidated,
}
#[derive(Debug)]
pub struct MailboxTurnError<Io, Commit> {
    pub progress: Box<MailboxProgress>,
    pub kind: MailboxTurnErrorKind<Io, Commit>,
}
fn valid_page(
    page: &MailboxDeltaPage,
    scope: ScopeId,
    previous: Option<&MailboxCursor>,
    limit: usize,
) -> bool {
    let c = &page.cursor;
    if !page.is_well_formed(scope, limit) {
        return false;
    }
    let after = previous.map_or(0, |p| p.after);
    if !page.complete && c.after <= after {
        return false;
    }
    if !c
        .after
        .checked_sub(after)
        .is_some_and(|diff| i64::try_from(page.ids.len()).is_ok_and(|n| diff >= n))
    {
        return false;
    }
    if let Some(p) = previous {
        if p.store_uuid != c.store_uuid
            || p.scope != c.scope
            || p.epoch != c.epoch
            || c.after < p.after
            || c.revision < p.revision
        {
            return false;
        }
        if let Some(window) = p.window {
            if page.complete {
                if (c.after, c.revision) != window {
                    return false;
                }
            } else if c.window != Some(window) || c.revision != p.revision {
                return false;
            }
        }
    }
    true
}
/// Commits a complete admitted page and its cursor together, synchronously.
/// The host authenticates records and policy in that callback. Failure/cancellation
/// cannot acknowledge the current page; completed earlier pages remain durable.
/// `work.bytes` measures raw payload bytes, independent of any transport encoding.
pub async fn sync_turn<T: MailboxTransport + ?Sized, Commit>(
    transport: &mut T,
    scope: ScopeId,
    prior: Option<MailboxCursor>,
    limits: MailboxTurnLimits,
    mut commit: impl FnMut(&[Record], &MailboxCursor) -> Result<(), Commit>,
) -> Result<MailboxProgress, MailboxTurnError<T::Error, Commit>> {
    let mut progress = MailboxProgress {
        cursor: prior,
        ..Default::default()
    };
    macro_rules! fail {
        ($kind:expr) => {
            return Err(MailboxTurnError {
                progress: Box::new(progress),
                kind: $kind,
            })
        };
    }
    if limits.work.records == 0
        || limits.work.bytes == 0
        || limits.work.requests == 0
        || limits.pages == 0
        || limits.page_records == 0
        || progress.cursor.as_ref().is_some_and(|c| {
            c.scope != scope
                || c.epoch < 0
                || c.after < 0
                || c.revision < 0
                || c.window
                    .is_some_and(|(upper, revision)| upper <= c.after || revision < c.revision)
        })
    {
        fail!(MailboxTurnErrorKind::InvalidLimits);
    }
    while progress.pages < limits.pages
        && progress.work.requests < limits.work.requests
        && progress.work.committed < limits.work.records
        && progress.work.bytes < limits.work.bytes
    {
        let limit = limits
            .page_records
            .min(limits.work.records - progress.work.committed);
        progress.work.requests += 1;
        let page = match transport
            .delta(
                scope,
                progress.cursor.as_ref(),
                limit,
                limits.work.bytes - progress.work.bytes,
            )
            .await
        {
            Ok(page) => page,
            Err(e) => fail!(MailboxTurnErrorKind::Transport(e)),
        };
        progress.pages += 1;
        if page.reset {
            fail!(MailboxTurnErrorKind::CheckpointInvalidated);
        }
        if !valid_page(&page, scope, progress.cursor.as_ref(), limit) {
            fail!(MailboxTurnErrorKind::InvalidPage);
        }
        let available = limits.work.bytes - progress.work.bytes;
        let records = if page.ids.is_empty() {
            vec![]
        } else {
            if progress.work.requests == limits.work.requests {
                return Ok(progress);
            }
            progress.work.requests += 1;
            match transport
                .bodies(scope, &page.cursor, &page.ids, available)
                .await
            {
                Ok(records) => records,
                Err(e) => fail!(MailboxTurnErrorKind::Transport(e)),
            }
        };
        let bytes = match crate::model::validate_batch(scope, &page.ids, &records, available) {
            Ok(bytes) => bytes,
            Err(crate::model::BatchError::Records) => fail!(MailboxTurnErrorKind::InvalidRecords),
            Err(crate::model::BatchError::Bytes) => fail!(MailboxTurnErrorKind::BodyLimit),
        };
        if let Err(e) = commit(&records, &page.cursor) {
            fail!(MailboxTurnErrorKind::Commit(e));
        }
        progress.work.committed += records.len();
        progress.work.bytes += bytes;
        progress.cursor = Some(page.cursor);
        if page.complete {
            progress.work.complete = true;
            return Ok(progress);
        }
    }
    Ok(progress)
}
impl<I: std::fmt::Display, C: std::fmt::Display> std::fmt::Display for MailboxTurnError<I, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "mailbox turn stopped after {} committed records: ",
            self.progress.work.committed
        )?;
        match &self.kind {
            MailboxTurnErrorKind::InvalidLimits => f.write_str("invalid limits"),
            MailboxTurnErrorKind::Transport(e) => write!(f, "transport: {e}"),
            MailboxTurnErrorKind::InvalidPage => f.write_str("invalid page"),
            MailboxTurnErrorKind::InvalidRecords => f.write_str("invalid records"),
            MailboxTurnErrorKind::BodyLimit => f.write_str("payload byte limit"),
            MailboxTurnErrorKind::Commit(e) => write!(f, "commit: {e}"),
            MailboxTurnErrorKind::CheckpointInvalidated => f.write_str("checkpoint invalidated"),
        }
    }
}
impl<I: std::error::Error + 'static, C: std::error::Error + 'static> std::error::Error
    for MailboxTurnError<I, C>
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            MailboxTurnErrorKind::Transport(e) => Some(e),
            MailboxTurnErrorKind::Commit(e) => Some(e),
            _ => None,
        }
    }
}
