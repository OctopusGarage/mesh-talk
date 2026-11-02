//! Bounded active retry stacks; durable signed-control cursors remain authority.
//! Restart drops unfinished stacks and resumes the same uncommitted candidate.
use super::*;
use super::{
    continuation::HistoryQuery,
    selection::{ReceiveSelection, SendSelection},
};
const ACTIVE: usize = 2 * MAX_RETRIES;
#[derive(Default)]
pub(in crate::node) struct HistoryWork {
    pub(super) receive: HashMap<ConversationId, ReceiveWork>,
    pub(super) send: HashMap<ConversationId, HistoryQuery<(Option<SendSelection>, bool)>>,
    pub(super) sender_after: Option<ConversationId>,
}
pub(super) struct ReceiveWork {
    pub(super) event: EventId,
    pub(super) rank: i64,
    pub(super) stamp: i64,
    pub(super) reopening: bool,
    pub(super) name: String,
    pub(super) query: HistoryQuery<Option<ReceiveSelection>>,
}
impl HistoryWork {
    #[cfg(test)]
    pub(in crate::node) fn is_idle(&self) -> bool {
        self.receive.is_empty() && self.send.is_empty()
    }

    pub(super) fn prune(&mut self, checkpoints: &ChannelCheckpointStore) -> Result<(), LogError> {
        let mut stale = Vec::new();
        for (&channel, work) in &self.receive {
            match checkpoints.check_source(&work.query.source) {
                Ok(()) => {}
                Err(LogError::StaleCursor) => stale.push(channel),
                Err(error) => return Err(error),
            }
        }
        for channel in stale {
            self.receive.remove(&channel);
        }
        let mut stale = Vec::new();
        for (&channel, work) in &self.send {
            match checkpoints.check_source(&work.source) {
                Ok(()) => {}
                Err(LogError::StaleCursor) => stale.push(channel),
                Err(error) => return Err(error),
            }
        }
        for channel in stale {
            self.send.remove(&channel);
        }
        Ok(())
    }
    pub(super) fn receive_full(&self) -> bool {
        self.receive.len() >= ACTIVE
    }
    pub(super) fn send_full(&self) -> bool {
        self.send.len() >= ACTIVE
    }
}
