//! Runtime-local retry pacing. These hints never retire or confirm durable work.

use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    time::{Duration, Instant},
};

const CAPACITY: usize = 256;

struct Entry<R> {
    routes: R,
    failures: u32,
    next: Instant,
    touched: Instant,
}

pub(super) struct RetrySchedule<K, R> {
    entries: HashMap<K, Entry<R>>,
}

impl<K, R> Default for RetrySchedule<K, R> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }
}

impl<K: Eq + Hash + Clone, R: Eq + Clone> RetrySchedule<K, R> {
    pub(super) fn ready(&mut self, key: &K, routes: &R, now: Instant) -> bool {
        self.entries
            .get(key)
            .is_none_or(|e| e.routes != *routes || now >= e.next)
    }
    pub(super) fn finished(&mut self, key: K, routes: R, progress: bool, now: Instant) {
        let failures = if progress {
            0
        } else {
            self.entries
                .get(&key)
                .filter(|e| e.routes == routes)
                .map_or(1, |e| e.failures.saturating_add(1))
        };
        if !self.entries.contains_key(&key)
            && self.entries.len() >= CAPACITY
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.touched)
                .map(|(k, _)| k.clone())
        {
            self.entries.remove(&oldest);
        }
        let delay = if progress {
            Duration::from_millis(500)
        } else {
            let mut hasher = DefaultHasher::new();
            key.hash(&mut hasher);
            failures.hash(&mut hasher);
            (Duration::from_secs(1 << failures.saturating_sub(1).min(4))
                + Duration::from_millis(hasher.finish() % 251))
            .min(Duration::from_secs(15))
        };
        self.entries.insert(
            key,
            Entry {
                routes,
                failures,
                next: now + delay,
                touched: now,
            },
        );
    }
    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timer_wakes_do_not_slide_cooldown_and_unrelated_lanes_remain_ready() {
        let mut retries = RetrySchedule::default();
        let now = Instant::now();
        retries.finished((0, "alice"), 1, false, now);
        assert!(!retries.ready(&(0, "alice"), &1, now));
        assert!(retries.ready(&(1, "alice"), &1, now));
        assert!(retries.ready(&(0, "bob"), &1, now));
        for ms in [100, 250, 500, 900] {
            assert!(!retries.ready(&(0, "alice"), &1, now + Duration::from_millis(ms)));
        }
        assert!(retries.ready(&(0, "alice"), &1, now + Duration::from_millis(1250)));
    }
    #[test]
    fn failures_back_off_with_a_bound_and_progress_recovers_quickly() {
        let mut retries = RetrySchedule::default();
        let mut now = Instant::now();
        for _ in 0..12 {
            retries.finished("alice", 1, false, now);
            now += Duration::from_secs(15);
            assert!(retries.ready(&"alice", &1, now));
        }
        retries.finished("alice", 1, false, now);
        assert!(!retries.ready(&"alice", &1, now + Duration::from_secs(8)));
        assert!(retries.ready(&"alice", &2, now));
        retries.finished("alice", 2, true, now);
        assert!(retries.ready(&"alice", &2, now + Duration::from_secs(1)));
        retries.clear();
        assert!(retries.ready(&"alice", &2, now));
    }
    #[test]
    fn failed_peer_inventory_is_bounded() {
        let mut retries = RetrySchedule::default();
        let now = Instant::now();
        for peer in 0..400 {
            retries.finished(peer, 1, false, now);
        }
        assert_eq!(retries.entries.len(), CAPACITY);
    }
}
