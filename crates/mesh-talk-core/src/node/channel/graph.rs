//! Channel authority reads exact signed evidence through one graph contract.
//! The slice adapter remains for pure validation; the runtime uses indexed reads.
use crate::eventlog::{Event, EventId, LogError};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
};
pub(super) mod preparing;

/// A historical predicate resumes after its last completely checked fact.
/// The pure graph has no persistent cursor; the production preparing graph does.
pub(crate) enum PredicateProgress {
    Scan(Option<Vec<u8>>),
    Complete(bool),
    Pending,
}

pub(crate) trait AuthorityGraph {
    fn event(&self, id: EventId) -> Result<Option<Cow<'_, Event>>, LogError>;
    /// Includes equality, matching signed account-authority ancestry checks.
    fn descends(&self, child: EventId, ancestor: EventId) -> Result<bool, LogError>;
    fn pending(&self) -> bool {
        false
    }
    fn predicate(&self, _key: &[u8]) -> Result<PredicateProgress, LogError> {
        Ok(PredicateProgress::Scan(None))
    }
    fn save_predicate(
        &self,
        _key: &[u8],
        _after: Option<&[u8]>,
        _answer: Option<bool>,
    ) -> Result<(), LogError> {
        Ok(())
    }
    fn admit_fact(&self, _bytes: usize) -> Result<bool, LogError> {
        Ok(true)
    }
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

/// A quantified historical predicate owns its last fully examined key. An
/// unresolved graph answer never advances that cursor or publishes a result.
pub(in crate::node) fn any_fact<T>(
    graph: &impl AuthorityGraph,
    key: &[u8],
    mut next: impl FnMut(Option<&[u8]>) -> Result<Option<(Vec<u8>, T)>, LogError>,
    mut matches: impl FnMut(T) -> Result<bool, LogError>,
) -> Result<bool, LogError> {
    let mut after = match graph.predicate(key)? {
        PredicateProgress::Complete(answer) => return Ok(answer),
        PredicateProgress::Pending => return Ok(false),
        PredicateProgress::Scan(after) => after,
    };
    loop {
        if !graph.admit_fact(256)? {
            return Ok(false);
        }
        let Some((cursor, fact)) = next(after.as_deref())? else {
            graph.save_predicate(key, after.as_deref(), Some(false))?;
            return Ok(false);
        };
        let matched = matches(fact)?;
        if graph.pending() {
            return Ok(false);
        }
        graph.save_predicate(key, Some(&cursor), matched.then_some(true))?;
        if matched {
            return Ok(true);
        }
        after = Some(cursor);
    }
}
