//! Participant accessors. Collectors are explicit compatibility operations;
//! ordinary event reads and network export do not retain their results here.
use super::pages::PageBudget;
use super::*;
use std::collections::{HashMap, HashSet};
impl EventRepository {
    pub(crate) fn sync(&self) -> Result<(), LogError> {
        // FULL-synchronous transactions already committed their WAL. Retain the
        // delivery boundary's fault check without a checkpoint or history scan.
        self.database.read(|_| Ok(()))
    }
    pub(crate) fn append_with_precommit(
        &self,
        event: &Event,
        precommit: impl FnOnce() -> Result<(), LogError>,
    ) -> Result<AppendOutcome, LogError> {
        self.database.transaction(|tx| {
            match self.append_with_gate_on(tx, event, |_| {
                precommit()?;
                Ok(AppendGate::<std::convert::Infallible>::Accept)
            })? {
                GatedAppend::Accepted(outcome) => Ok(outcome),
                GatedAppend::Blocked(never) => match never {},
            }
        })
    }
    pub(crate) fn collect_events(
        &self,
        conversation: &ConversationId,
    ) -> Result<Vec<Event>, LogError> {
        self.project_events(conversation, |event| event)
    }
    pub(crate) fn project_events<T>(
        &self,
        conversation: &ConversationId,
        mut project: impl FnMut(Event) -> T,
    ) -> Result<Vec<T>, LogError> {
        let mut result = Vec::new();
        let mut after = None;
        let mut budget = PageBudget::default();
        loop {
            let page = match self.events_page(conversation, after.as_ref(), budget) {
                Err(LogError::PageBudgetTooSmall { required }) => {
                    budget = PageBudget {
                        rows: 1,
                        bytes: required,
                    };
                    continue;
                }
                result => result?,
            };
            result.extend(page.items.into_iter().map(&mut project));
            after = page.next;
            if after.is_none() {
                return Ok(result);
            }
            budget = PageBudget::default();
        }
    }
    pub(crate) fn collect_conversations(&self) -> Result<Vec<ConversationId>, LogError> {
        let mut result = Vec::new();
        let mut after = None;
        loop {
            let page = self.conversations_page(after.as_ref(), PageBudget::default())?;
            result.extend(page.items.into_iter().map(|m| m.conversation));
            after = page.next;
            if after.is_none() {
                return Ok(result);
            }
        }
    }
    pub(crate) fn collect_ids(&self) -> Result<Vec<EventId>, LogError> {
        let mut result = Vec::new();
        for conversation in self.collect_conversations()? {
            let mut after = None;
            loop {
                let page = self.ids_page(&conversation, after.as_ref(), PageBudget::default())?;
                result.extend(page.items);
                after = page.next;
                if after.is_none() {
                    break;
                }
            }
        }
        Ok(result)
    }
    pub(crate) fn collect_author_maxima(
        &self,
        conversation: &ConversationId,
    ) -> Result<HashMap<Author, u64>, LogError> {
        let mut result = HashMap::new();
        let mut after = None;
        loop {
            let page =
                self.author_maxima_page(conversation, after.as_ref(), PageBudget::default())?;
            result.extend(page.items.into_iter().map(|m| (m.author, m.max_seq)));
            after = page.next;
            if after.is_none() {
                return Ok(result);
            }
        }
    }
    pub(crate) fn message_keys_after(
        &self,
        conversation: ConversationId,
        author: Author,
        after: Option<(u64, EventId)>,
        limit: usize,
    ) -> Result<Vec<(u64, EventId)>, LogError> {
        self.database.read(|c| {
            self.require_active_on(c,&conversation)?;
            let condition=if after.is_some() {"AND (lamport,id)>(?4,?5)"} else {""};
            let mut stmt=c.prepare(&format!("SELECT lamport,id FROM retained_events WHERE domain=?1 AND conversation=?2 AND author=?3 AND kind=0 {condition} ORDER BY lamport,id LIMIT ?6"))?;
            let mut rows=stmt.query(params![self.domain.0.as_slice(),conversation.as_bytes().as_slice(),author.ed25519_pub().as_slice(),after.map_or(0,|v|v.0).to_be_bytes().as_slice(),after.map_or([0;32],|v|*v.1.as_bytes()).as_slice(),integer(limit)?])?;
            let mut result=Vec::new(); while let Some(row)=rows.next()? { result.push((u64::from_be_bytes(blob(row,0)?),EventId::new(blob(row,1)?))); } Ok(result)
        })
    }
    pub(crate) fn retain_conversations(
        &self,
        keep: &HashSet<ConversationId>,
    ) -> Result<usize, LogError> {
        let conversations = self.collect_conversations()?;
        self.database.transaction(|tx| {
            let mut removed = 0usize;
            for conversation in conversations {
                if !keep.contains(&conversation) {
                    removed = removed
                        .checked_add(self.drop_conversation_on(tx, &conversation)?)
                        .ok_or(LogError::RepositoryCounterExhausted)?;
                }
            }
            Ok(removed)
        })
    }
    #[cfg(test)]
    pub(crate) fn before_mutation(&mut self, hook: Box<dyn FnOnce() + Send + Sync>) {
        *self.before_mutation.get_mut().unwrap() = Some(hook);
    }
}
