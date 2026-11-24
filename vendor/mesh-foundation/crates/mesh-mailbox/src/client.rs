//! One bounded turn on an already negotiated authenticated mailbox session.
//!
//! A page cursor is acknowledged only with its entire batch. The commit callback
//! must authenticate records, recheck authorization, and atomically persist bodies
//! plus cursor. A complete result covers this source snapshot, not group convergence.

use crate::rpc::{self, BODY_BUDGET, Frame, MailboxCursor, RpcError};
use mesh_replication::{
    legacy_event::{ConversationId, Event},
    pull::{PullProgress, PullTurnLimits},
    session::ReconciliationSession,
};

#[derive(Clone, Copy, Debug)]
pub struct MailboxTurnLimits {
    pub work: PullTurnLimits,
    pub pages: usize,
}
impl Default for MailboxTurnLimits {
    fn default() -> Self {
        Self {
            work: PullTurnLimits::default(),
            pages: 8,
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
    Wire(RpcError),
    Commit(Commit),
    /// This legacy page cannot be acknowledged partially. Increase the record
    /// allowance to at least one page; never discard unacknowledged IDs.
    PageExceedsTurn,
    /// The server invalidated the source-local checkpoint. Reauthorize and start
    /// a bounded repair from no cursor; no cursor was acknowledged by this turn.
    CheckpointInvalidated,
}
#[derive(Debug)]
pub struct MailboxTurnError<Io, Commit> {
    pub progress: Box<MailboxProgress>,
    pub kind: MailboxTurnErrorKind<Io, Commit>,
}

pub async fn sync_turn<S: ReconciliationSession, Commit>(
    session: &mut S,
    scope: ConversationId,
    prior: Option<MailboxCursor>,
    limits: MailboxTurnLimits,
    mut commit: impl FnMut(&[Event], &MailboxCursor) -> Result<(), Commit>,
) -> Result<MailboxProgress, MailboxTurnError<S::Error, Commit>> {
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
        || limits.work.bytes < BODY_BUDGET
        || limits.work.requests < 2
        || limits.pages == 0
        || progress
            .cursor
            .as_ref()
            .is_some_and(|c| c.conversation != scope)
    {
        fail!(MailboxTurnErrorKind::InvalidLimits);
    }
    while progress.pages < limits.pages && progress.work.requests < limits.work.requests {
        let request = match rpc::encode_frame(&Frame::Delta {
            conversation: scope,
            prior: progress.cursor.clone(),
        }) {
            Ok(request) => request,
            Err(error) => fail!(MailboxTurnErrorKind::Wire(error)),
        };
        progress.work.requests += 1;
        if let Err(error) = session.send(&request).await {
            fail!(MailboxTurnErrorKind::Transport(error));
        }
        let response = match session.recv().await {
            Ok(response) => response,
            Err(error) => fail!(MailboxTurnErrorKind::Transport(error)),
        };
        let page = match rpc::read_delta_response(&response, scope) {
            Ok(page) => page,
            Err(error) => fail!(MailboxTurnErrorKind::Wire(error)),
        };
        progress.pages += 1;
        if page.reset {
            fail!(MailboxTurnErrorKind::CheckpointInvalidated);
        }
        let previous = progress.cursor.as_ref();
        let identity_changed = previous.is_some_and(|p| {
            p.store_uuid != page.cursor.store_uuid
                || p.epoch != page.cursor.epoch
                || p.first_seen != page.cursor.first_seen
        });
        let changed_window = previous.and_then(|p| p.window).is_some_and(|window| {
            if page.complete {
                page.cursor.after != window.0 || page.cursor.revision != window.1
            } else {
                page.cursor.window != Some(window)
                    || previous.is_some_and(|p| page.cursor.revision != p.revision)
            }
        });
        let invalid = changed_window
            || (!page.reset
                && (identity_changed
                    || previous.is_some_and(|p| {
                        page.cursor.after < p.after || page.cursor.revision < p.revision
                    })))
            || (!page.complete && previous == Some(&page.cursor))
            || page.cursor.window.is_some_and(|(upper, revision)| {
                upper < page.cursor.after || revision < page.cursor.revision
            })
            || page
                .ids
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != page.ids.len();
        if invalid {
            fail!(MailboxTurnErrorKind::Wire(RpcError::UnexpectedMessage));
        }
        if page.ids.len() > limits.work.records - progress.work.committed {
            if progress.work.committed == 0 {
                fail!(MailboxTurnErrorKind::PageExceedsTurn);
            }
            return Ok(progress);
        }
        let events = if page.ids.is_empty() {
            Vec::new()
        } else {
            // Old Get frames cannot advertise a smaller byte budget: reserve a
            // full protocol response before requesting any bodies.
            if limits.work.bytes - progress.work.bytes < BODY_BUDGET
                || progress.work.requests == limits.work.requests
            {
                return Ok(progress);
            }
            let request = match rpc::make_get_request(scope, &page.cursor, &page.ids) {
                Ok(request) => request,
                Err(error) => fail!(MailboxTurnErrorKind::Wire(error)),
            };
            progress.work.requests += 1;
            if let Err(error) = session.send(&request).await {
                fail!(MailboxTurnErrorKind::Transport(error));
            }
            let response = match session.recv().await {
                Ok(response) => response,
                Err(error) => fail!(MailboxTurnErrorKind::Transport(error)),
            };
            match rpc::read_get_response(&response, scope, &page.ids, event_size) {
                Ok(events) => events,
                Err(error) => fail!(MailboxTurnErrorKind::Wire(error)),
            }
        };
        let bytes = events.iter().map(event_size).sum::<usize>();
        if let Err(error) = commit(&events, &page.cursor) {
            fail!(MailboxTurnErrorKind::Commit(error));
        }
        progress.work.committed += events.len();
        progress.work.bytes += bytes;
        progress.cursor = Some(page.cursor);
        if page.complete {
            progress.work.complete = true;
            return Ok(progress);
        }
        if progress.work.committed == limits.work.records {
            return Ok(progress);
        }
    }
    Ok(progress)
}
fn event_size(event: &Event) -> usize {
    bincode::serialized_size(event)
        .ok()
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(usize::MAX)
}

impl<Io: std::fmt::Display, Commit: std::fmt::Display> std::fmt::Display
    for MailboxTurnError<Io, Commit>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "mailbox turn stopped after {} committed records: ",
            self.progress.work.committed
        )?;
        match &self.kind {
            MailboxTurnErrorKind::InvalidLimits => f.write_str("invalid turn limits"),
            MailboxTurnErrorKind::Transport(error) => write!(f, "transport: {error}"),
            MailboxTurnErrorKind::Wire(error) => write!(f, "frame: {error}"),
            MailboxTurnErrorKind::Commit(error) => write!(f, "commit: {error}"),
            MailboxTurnErrorKind::PageExceedsTurn => {
                f.write_str("page exceeds the turn record allowance")
            }
            MailboxTurnErrorKind::CheckpointInvalidated => {
                f.write_str("source checkpoint invalidated")
            }
        }
    }
}
impl<Io: std::error::Error + 'static, Commit: std::error::Error + 'static> std::error::Error
    for MailboxTurnError<Io, Commit>
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            MailboxTurnErrorKind::Transport(error) => Some(error),
            MailboxTurnErrorKind::Wire(error) => Some(error),
            MailboxTurnErrorKind::Commit(error) => Some(error),
            _ => None,
        }
    }
}
