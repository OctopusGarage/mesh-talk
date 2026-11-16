//! Bounded public fanout scheduling; durable work remains in the source journal.
use crate::eventlog::ConversationId;
use std::collections::{HashMap, HashSet, VecDeque};

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
    retry_at: HashMap<ConversationId, (tokio::time::Instant, u64)>,
}

/// Roster observations include heartbeats. Only new full identities or changed
/// endpoint sets release retry cooldowns; timestamps and names grant no credit.
type RouteIdentity = ([u8; 32], [u8; 32]);
type RouteValue = (Vec<std::net::SocketAddr>, Option<String>, bool);
type RouteObservation = (RouteValue, Option<std::time::Instant>);

#[derive(Default)]
pub(super) struct FanoutRoutes {
    known: HashMap<RouteIdentity, RouteObservation>,
}
impl FanoutRoutes {
    pub(super) fn observe(
        &mut self,
        peers: impl Iterator<Item = (RouteIdentity, RouteObservation)>,
        now: std::time::Instant,
    ) -> HashSet<RouteIdentity> {
        let current: HashMap<_, _> = peers
            .map(
                |(identity, ((mut routes, account, post_office), deadline))| {
                    routes.sort_unstable();
                    routes.dedup();
                    (identity, ((routes, account, post_office), deadline))
                },
            )
            .collect();
        let changed = current
            .iter()
            .filter_map(|(identity, value)| {
                self.known
                    .get(identity)
                    .is_none_or(|old| {
                        old.0 != value.0
                            || (old.1.is_none_or(|at| at <= now)
                                && value.1.is_some_and(|at| at > now))
                    })
                    .then_some(*identity)
            })
            .collect();
        self.known = current;
        changed
    }
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
        self.retry_at.remove(&group);
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
            .position(|group| self.retry_at.get(group).is_none_or(|(at, _)| *at <= now))?;
        let group = self.queued.remove(index)?;
        self.active.insert(group, false);
        Some(group)
    }

    /// Keep the observed revision pending after local admission backpressure.
    /// Overflow returns ownership to the existing durable journal cursor.
    pub(super) fn defer(&mut self, group: ConversationId) -> bool {
        let refreshed = self.active.remove(&group) == Some(true);
        if self.queued.len() >= CAPACITY {
            self.seen.remove(&group);
            self.retry_at.remove(&group);
            return false;
        }
        self.queued.push_back(group);
        // Keep failure history only while this exact revision is pending.
        // A newer revision or semantic route wake during the worker wins over
        // the old completion and must not inherit its cooldown.
        if !refreshed {
            let seconds = self
                .retry_at
                .get(&group)
                .map_or(1, |(_, seconds)| (seconds * 2).min(300));
            self.retry_at.insert(
                group,
                (
                    tokio::time::Instant::now() + std::time::Duration::from_secs(seconds),
                    seconds,
                ),
            );
        }
        true
    }

    pub(super) fn next_retry(&self) -> Option<tokio::time::Instant> {
        if self.active.len() >= WORKERS {
            return None;
        }
        self.queued
            .iter()
            .filter_map(|group| self.retry_at.get(group).map(|(at, _)| *at))
            .min()
    }

    /// Coalesced semantic dependency changes release pending work, including
    /// one follow-up when a now-obsolete round is still owned by its worker.
    pub(super) fn wake_pending(&mut self) {
        self.wake_pending_for(|_| true);
    }

    pub(super) fn wake_pending_for(&mut self, mut affected: impl FnMut(ConversationId) -> bool) {
        self.retry_at.retain(|group, _| !affected(*group));
        for (group, refreshed) in &mut self.active {
            if affected(*group) {
                *refreshed = true;
            }
        }
    }

    /// A newer committed effect during an active round needs one follow-up.
    /// Forget its observation so the caller can requeue the journal's latest row.
    pub(super) fn finished(&mut self, group: ConversationId) -> bool {
        self.retry_at.remove(&group);
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

    #[tokio::test(start_paused = true)]
    async fn repeated_deferral_backs_off_without_identical_observation_reset() {
        let mut queue = FanoutQueue::default();
        let first = group(1);
        queue.observe(first, 2);
        for seconds in [1, 2, 4, 8, 16, 32, 64, 128, 256, 300, 300] {
            assert_eq!(queue.start_next(), Some(first));
            assert!(queue.next_retry().is_none(), "active retry must not spin");
            assert!(queue.defer(first));
            queue.observe(first, 2);
            queue.observe(first, 1);
            assert_eq!(
                queue.next_retry().unwrap() - tokio::time::Instant::now(),
                std::time::Duration::from_secs(seconds)
            );
            tokio::time::advance(std::time::Duration::from_secs(seconds)).await;
        }
        assert_eq!(queue.start_next(), Some(first));
        assert!(!queue.finished(first));
        assert!(queue.next_retry().is_none());
        queue.observe(first, 2);
        assert_eq!(queue.start_next(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn newer_revision_and_semantic_wake_override_old_deferred_completion() {
        let mut queue = FanoutQueue::default();
        let first = group(1);
        queue.observe(first, 1);
        queue.start_next();
        queue.defer(first);
        queue.observe(first, 2);
        assert_eq!(queue.start_next(), Some(first));
        queue.observe(first, 3);
        queue.defer(first);
        assert_eq!(queue.start_next(), Some(first));
        queue.defer(first);
        queue.wake_pending();
        assert_eq!(queue.start_next(), Some(first));
        queue.wake_pending();
        queue.defer(first);
        assert_eq!(queue.start_next(), Some(first));
        queue.defer(first);
        assert_eq!(queue.retry_at.len(), 1);
        assert_eq!(
            queue.next_retry().unwrap() - tokio::time::Instant::now(),
            std::time::Duration::from_secs(1)
        );
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        queue.start_next();
        queue.finished(first);
        assert!(queue.retry_at.is_empty());
    }

    #[test]
    fn route_wake_ignores_heartbeats_and_remembers_removal_for_restoration() {
        let mut routes = FanoutRoutes::default();
        let now = std::time::Instant::now();
        let identity = ([1; 32], [2; 32]);
        let mut peer = (
            identity,
            ((vec!["127.0.0.1:1".parse().unwrap()], None, false), None),
        );
        assert_eq!(
            routes.observe([peer.clone()].into_iter(), now),
            HashSet::from([identity])
        );
        assert!(routes.observe([peer.clone()].into_iter(), now).is_empty());
        assert!(routes.observe(std::iter::empty(), now).is_empty());
        assert_eq!(
            routes.observe([peer.clone()].into_iter(), now),
            HashSet::from([identity])
        );
        peer.1.0.1 = Some("new account".into());
        assert_eq!(
            routes.observe([peer.clone()].into_iter(), now),
            HashSet::from([identity])
        );
        peer.1.0.2 = true;
        assert_eq!(
            routes.observe([peer.clone()].into_iter(), now),
            HashSet::from([identity])
        );
        peer.0.1 = [3; 32];
        assert_eq!(
            routes.observe([peer.clone()].into_iter(), now),
            HashSet::from([peer.0])
        );
    }

    #[test]
    fn presence_return_wakes_without_an_intermediate_offline_observation() {
        let mut routes = FanoutRoutes::default();
        let now = std::time::Instant::now();
        let identity = ([1; 32], [2; 32]);
        let mut peer = (
            identity,
            (
                (vec!["127.0.0.1:1".parse().unwrap()], None, false),
                Some(now + std::time::Duration::from_secs(15)),
            ),
        );
        assert!(
            routes
                .observe([peer.clone()].into_iter(), now)
                .contains(&identity)
        );
        peer.1.1 = Some(now + std::time::Duration::from_secs(20));
        assert!(
            routes
                .observe(
                    [peer.clone()].into_iter(),
                    now + std::time::Duration::from_secs(5)
                )
                .is_empty()
        );
        peer.1.1 = Some(now + std::time::Duration::from_secs(40));
        assert!(
            routes
                .observe(
                    [peer.clone()].into_iter(),
                    now + std::time::Duration::from_secs(25)
                )
                .contains(&identity)
        );
        peer.1.1 = Some(now + std::time::Duration::from_secs(45));
        assert!(
            routes
                .observe([peer].into_iter(), now + std::time::Duration::from_secs(30))
                .is_empty()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unrelated_peer_wake_preserves_other_group_cooldown() {
        let mut queue = FanoutQueue::default();
        let first = group(1);
        let second = group(2);
        for group in [first, second] {
            queue.observe(group, 1);
            assert_eq!(queue.start_next(), Some(group));
            queue.defer(group);
        }
        let original = queue.retry_at[&second];
        queue.wake_pending_for(|group| group == first);
        assert_eq!(queue.retry_at[&second], original);
        assert_eq!(queue.start_next(), Some(first));
        assert_eq!(queue.start_next(), None);
        queue.wake_pending_for(|group| group == second);
        assert_eq!(queue.start_next(), Some(second));
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
