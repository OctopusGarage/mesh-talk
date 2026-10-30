//! Bounded public fanout scheduling; durable work remains in the source journal.
use crate::eventlog::ConversationId;
use std::collections::{HashMap, VecDeque};

pub(super) const CAPACITY: usize = 128;
pub(super) const WORKERS: usize = 2;

/// Only the source journal's bounded group index supplies observations. Remembered
/// sequences therefore have at most its 100,000 distinct source groups. No event
/// bodies, transmission confirmations or additional durable jobs live here.
#[derive(Default)]
pub(super) struct FanoutQueue {
    queued: VecDeque<ConversationId>,
    active: HashMap<ConversationId, bool>,
    seen: HashMap<ConversationId, u64>,
    retry_at: HashMap<ConversationId, tokio::time::Instant>,
}

impl FanoutQueue {
    /// False leaves this new revision unobserved so the durable cursor retries it.
    pub(super) fn observe(&mut self, group: ConversationId, sequence: u64) -> bool {
        if self
            .seen
            .get(&group)
            .is_some_and(|known| *known >= sequence)
        {
            return true;
        }
        if let Some(refresh) = self.active.get_mut(&group) {
            *refresh = true;
        } else if !self.queued.contains(&group) {
            if self.queued.len() >= CAPACITY {
                return false;
            }
            self.queued.push_back(group);
        }
        self.seen.insert(group, sequence);
        true
    }

    pub(super) fn start_next(&mut self) -> Option<ConversationId> {
        if self.active.len() >= WORKERS {
            return None;
        }
        let now = tokio::time::Instant::now();
        let index = self
            .queued
            .iter()
            .position(|group| self.retry_at.get(group).is_none_or(|at| *at <= now))?;
        let group = self.queued.remove(index)?;
        self.retry_at.remove(&group);
        self.active.insert(group, false);
        Some(group)
    }

    /// Keep the observed revision pending after local admission backpressure.
    /// Overflow returns ownership to the existing durable journal cursor.
    pub(super) fn defer(&mut self, group: ConversationId) -> bool {
        self.active.remove(&group);
        if self.queued.len() >= CAPACITY {
            self.seen.remove(&group);
            return false;
        }
        self.queued.push_back(group);
        self.retry_at.insert(
            group,
            tokio::time::Instant::now() + std::time::Duration::from_secs(1),
        );
        true
    }

    pub(super) fn next_retry(&self) -> Option<tokio::time::Instant> {
        if self.active.len() >= WORKERS {
            return None;
        }
        self.retry_at.values().copied().min()
    }

    /// A newer committed effect during an active round needs one follow-up.
    /// Forget its observation so the caller can requeue the journal's latest row.
    pub(super) fn finished(&mut self, group: ConversationId) -> bool {
        if self.active.remove(&group) == Some(true) {
            self.seen.remove(&group);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(index: usize) -> ConversationId {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&(index as u64).to_be_bytes());
        ConversationId::new(bytes)
    }

    #[tokio::test(start_paused = true)]
    async fn deferred_revision_waits_without_blocking_ready_groups_and_dedupes_success() {
        let mut queue = FanoutQueue::default();
        let first = group(1);
        let second = group(2);
        assert!(queue.observe(first, 1));
        assert_eq!(queue.start_next(), Some(first));
        assert!(queue.defer(first));
        assert!(queue.observe(first, 1));
        assert!(queue.observe(second, 1));
        assert_eq!(queue.start_next(), Some(second));
        assert!(!queue.finished(second));
        assert_eq!(queue.start_next(), None);
        let retry = queue.next_retry().unwrap();
        tokio::time::advance(std::time::Duration::from_millis(999)).await;
        assert_eq!(queue.start_next(), None);
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        assert_eq!(tokio::time::Instant::now(), retry);
        assert_eq!(queue.start_next(), Some(first));
        assert!(!queue.finished(first));
        assert!(queue.observe(first, 1));
        assert_eq!(queue.start_next(), None);
        assert!(queue.next_retry().is_none());
    }

    #[test]
    fn deferred_overflow_returns_same_revision_to_the_durable_cursor() {
        let mut queue = FanoutQueue::default();
        let first = group(CAPACITY + 1);
        assert!(queue.observe(first, 1));
        assert_eq!(queue.start_next(), Some(first));
        for index in 0..CAPACITY {
            assert!(queue.observe(group(index), 1));
        }
        assert!(!queue.defer(first));
        assert_eq!(queue.queued.len(), CAPACITY);
        assert!(!queue.seen.contains_key(&first));
        assert!(queue.next_retry().is_none());
        assert_eq!(queue.start_next(), Some(group(0)));
        assert!(queue.observe(first, 1));
    }

    #[test]
    fn full_queue_preserves_the_oldest_unobserved_revision_for_retry() {
        let mut queue = FanoutQueue::default();
        for index in 0..CAPACITY {
            assert!(queue.observe(group(index), 1));
        }
        let waiting = group(CAPACITY);
        assert!(!queue.observe(waiting, 1));
        assert!(!queue.seen.contains_key(&waiting));
        assert_eq!(queue.start_next(), Some(group(0)));
        assert!(queue.observe(waiting, 1));
        assert_eq!(queue.start_next(), Some(group(1)));
        assert_eq!(
            queue.start_next(),
            None,
            "two actual workers exhaust admission"
        );
        assert!(!queue.finished(group(0)));
        for index in 2..=CAPACITY {
            assert_eq!(queue.start_next(), Some(group(index)));
            assert!(!queue.finished(group(index)));
        }
        assert_eq!(queue.start_next(), None);
        assert!(!queue.finished(group(1)));
    }

    #[test]
    fn queued_updates_coalesce_and_active_updates_request_one_followup() {
        let mut queue = FanoutQueue::default();
        let first = group(1);
        let second = group(2);
        assert!(queue.observe(first, 1));
        assert!(queue.observe(first, 3));
        assert!(queue.observe(first, 2));
        assert!(queue.observe(second, 1));
        assert_eq!(queue.queued.len(), 2);
        assert_eq!(queue.start_next(), Some(first));
        assert!(queue.observe(first, 3));
        assert_eq!(queue.active.get(&first), Some(&false));
        assert!(queue.observe(first, 4));
        assert!(queue.observe(first, 5));
        assert_eq!(queue.queued.len(), 1);
        assert!(queue.finished(first));
        assert!(!queue.seen.contains_key(&first));
        assert!(queue.observe(first, 5));
        assert_eq!(
            queue.start_next(),
            Some(second),
            "a refresh retains FIFO fairness"
        );
        assert_eq!(queue.start_next(), Some(first));
        assert!(!queue.finished(first));
        assert!(queue.observe(first, 5));
        assert_eq!(
            queue.start_next(),
            None,
            "the completed sequence is already observed"
        );
        assert!(!queue.finished(second));
    }

    #[test]
    fn an_unqueued_new_revision_is_not_lost_when_the_queue_is_full() {
        let mut queue = FanoutQueue::default();
        let previous = group(CAPACITY + 1);
        assert!(queue.observe(previous, 1));
        assert_eq!(queue.start_next(), Some(previous));
        assert!(!queue.finished(previous));
        for index in 0..CAPACITY {
            assert!(queue.observe(group(index), 1));
        }
        assert!(!queue.observe(previous, 2));
        assert_eq!(queue.seen.get(&previous), Some(&1));
        assert_eq!(queue.start_next(), Some(group(0)));
        assert!(queue.observe(previous, 2));
        assert_eq!(queue.seen.get(&previous), Some(&2));
    }
}
