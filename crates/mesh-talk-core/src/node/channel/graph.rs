//! Channel authority reads exact signed evidence through one graph contract.
//! The slice adapter remains for pure validation; the runtime uses indexed reads.
use crate::eventlog::{Event, EventId, LogError};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
};

pub(crate) trait AuthorityGraph {
    fn event(&self, id: EventId) -> Result<Option<Cow<'_, Event>>, LogError>;
    /// Includes equality, matching signed account-authority ancestry checks.
    fn descends(&self, child: EventId, ancestor: EventId) -> Result<bool, LogError>;
}
impl AuthorityGraph for HashMap<EventId, &Event> {
    fn event(&self, id: EventId) -> Result<Option<Cow<'_, Event>>, LogError> {
        Ok(self.get(&id).copied().map(Cow::Borrowed))
    }
    fn descends(&self, child: EventId, ancestor: EventId) -> Result<bool, LogError> {
        let mut pending = vec![child];
        let mut seen = HashSet::new();
        while let Some(id) = pending.pop() {
            if id == ancestor {
                return Ok(true);
            }
            if seen.insert(id)
                && let Some(event) = self.get(&id)
            {
                pending.extend(event.parents.iter().copied());
            }
        }
        Ok(false)
    }
}

pub(in crate::node) struct IndexedGraph<'a> {
    pub(in crate::node) repository: &'a crate::eventlog::repository::EventRepository,
    pub(in crate::node) conversation: crate::eventlog::ConversationId,
}
impl AuthorityGraph for IndexedGraph<'_> {
    fn event(&self, id: EventId) -> Result<Option<Cow<'_, Event>>, LogError> {
        Ok(self
            .repository
            .get(&id)?
            .filter(|event| event.conversation_id == self.conversation)
            .map(Cow::Owned))
    }
    fn descends(&self, child: EventId, ancestor: EventId) -> Result<bool, LogError> {
        self.repository
            .descends_from(self.conversation, child, ancestor)
    }
}

/// The source guard runs in the same transaction as projection/receiver writes.
/// An append may reuse the Kahn job, but still invalidates a prepared decision
/// until its new tail is projected. Retirement and a replacement job fail closed.
pub(in crate::node) fn require_source_on(
    c: &rusqlite::Connection,
    channel: crate::eventlog::ConversationId,
    job: [u8; 16],
    revision: i64,
) -> Result<(), LogError> {
    let valid:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM event_order_current o JOIN event_conversations e ON e.domain=o.domain AND e.conversation=o.conversation WHERE o.domain=?1 AND o.conversation=?2 AND o.order_kind=0 AND o.job=?3 AND o.source_revision=?4 AND e.revision=?4 AND e.lifecycle=0)",
        rusqlite::params![b"MeshPartEventsV1".as_slice(),channel.as_bytes().as_slice(),job.as_slice(),revision],|r|r.get(0))?;
    if !valid {
        return Err(LogError::StaleCursor);
    }
    Ok(())
}
