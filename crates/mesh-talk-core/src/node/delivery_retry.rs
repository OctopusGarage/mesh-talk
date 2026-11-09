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
    reservation: Option<u64>,
}

pub(super) struct RetrySchedule<K, R> {
    entries: HashMap<K, Entry<R>>,
    generation: u64,
    failure_floor: Duration,
    failure_ceiling: Duration,
}

impl<K, R> Default for RetrySchedule<K, R> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            generation: 0,
            failure_floor: Duration::ZERO,
            failure_ceiling: Duration::from_secs(15),
        }
    }
}

impl<K: Eq + Hash + Clone, R: Eq + Clone> RetrySchedule<K, R> {
    pub(super) fn with_failure_ceiling(failure_ceiling: Duration) -> Self {
        assert!(!failure_ceiling.is_zero());
        Self {
            failure_ceiling,
            ..Self::default()
        }
    }
    /// A recovery floor applies only after an observed remote failure, never
    /// to a reservation or local admission/cancellation deferral.
    pub(super) fn with_failure_bounds(floor: Duration, ceiling: Duration) -> Self {
        assert!(!floor.is_zero() && floor <= ceiling);
        Self {
            failure_floor: floor,
            ..Self::with_failure_ceiling(ceiling)
        }
    }
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
        let delay = if progress {
            Duration::from_millis(500)
        } else {
            Self::failure_delay(&key, failures, self.failure_floor, self.failure_ceiling)
        };
        self.record_completion(key, routes, failures, delay, now);
    }

    /// Successful absence of work is neither progress nor a failed attempt.
    /// Keep a finite repair deadline without turning a clean poll into a loop.
    pub(super) fn settled(&mut self, key: K, routes: R, delay: Duration, now: Instant) {
        self.record_completion(key, routes, 0, delay, now);
    }

    fn record_completion(
        &mut self,
        key: K,
        routes: R,
        failures: u32,
        delay: Duration,
        now: Instant,
    ) {
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
        self.entries.insert(
            key,
            Entry {
                routes,
                failures,
                next: now + delay,
                touched: now,
                reservation: None,
            },
        );
    }
    /// Reserve before spawning cancellable work. Unexpired hints cannot be
    /// displaced by inventory churn; callers defer admission when full.
    pub(super) fn reserve(&mut self, key: K, routes: R, now: Instant) -> Option<(u64, Option<K>)> {
        if !self.ready(&key, &routes, now) {
            return None;
        }
        let mut evicted = None;
        if !self.entries.contains_key(&key) && self.entries.len() >= CAPACITY {
            let oldest = self
                .entries
                .iter()
                .filter(|(_, entry)| entry.reservation.is_none() && entry.next <= now)
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, _)| key.clone())?;
            self.entries.remove(&oldest);
            evicted = Some(oldest);
        }
        let failures = self
            .entries
            .get(&key)
            .filter(|entry| entry.routes == routes)
            .map_or(0, |entry| entry.failures);
        // This counter intentionally survives clear/eviction: a late completion
        // must never acquire a replacement reservation with the same key/routes.
        self.generation = self.generation.checked_add(1)?;
        let token = self.generation;
        self.entries.insert(
            key.clone(),
            Entry {
                routes,
                failures,
                next: now
                    + Self::failure_delay(
                        &key,
                        failures.saturating_add(1),
                        Duration::ZERO,
                        self.failure_ceiling,
                    ),
                touched: now,
                reservation: Some(token),
            },
        );
        Some((token, evicted))
    }

    pub(super) fn owns(&self, key: &K, token: u64) -> bool {
        self.entries
            .get(key)
            .is_some_and(|entry| entry.reservation == Some(token))
    }

    pub(super) fn forget(&mut self, key: &K) {
        self.entries.remove(key);
    }

    /// Local admission pressure proves neither remote failure nor success.
    /// Retain learned failures and release only this attempt's reservation.
    pub(super) fn defer(&mut self, key: &K, token: u64, delay: Duration, now: Instant) {
        let Some(entry) = self
            .entries
            .get_mut(key)
            .filter(|entry| entry.reservation == Some(token))
        else {
            return;
        };
        entry.next = entry.next.max(now + delay);
        entry.touched = now;
        entry.reservation = None;
    }

    /// A clean protocol boundary clears pacing immediately, even if the phase
    /// did not change. Failed work gets a full deadline from actual completion.
    pub(super) fn complete(&mut self, key: &K, token: u64, clean: bool, now: Instant) {
        let Some(entry) = self
            .entries
            .get_mut(key)
            .filter(|entry| entry.reservation == Some(token))
        else {
            return;
        };
        if clean {
            self.entries.remove(key);
        } else {
            entry.failures = entry.failures.saturating_add(1);
            entry.next = now
                + Self::failure_delay(
                    key,
                    entry.failures,
                    self.failure_floor,
                    self.failure_ceiling,
                );
            entry.touched = now;
            entry.reservation = None;
        }
    }

    fn failure_delay(key: &K, failures: u32, floor: Duration, ceiling: Duration) -> Duration {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        failures.hash(&mut hasher);
        Duration::from_secs(1_u64 << failures.saturating_sub(1).min(63))
            .max(floor)
            .min(ceiling)
            .saturating_add(Duration::from_millis(hasher.finish() % 251))
            .min(ceiling)
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_deferral_preserves_failures_deadlines_and_exact_reservation_authority() {
        let mut retries = RetrySchedule::default();
        let mut now = Instant::now();
        for _ in 0..6 {
            retries.finished("peer", 1, false, now);
            now += Duration::from_secs(20);
        }
        let token = retries.reserve("peer", 1, now).unwrap().0;
        let deadline = retries.entries["peer"].next;
        let failures = retries.entries["peer"].failures;
        retries.defer(&"peer", token, Duration::from_secs(5), now);
        assert_eq!(retries.entries["peer"].failures, failures);
        assert_eq!(retries.entries["peer"].next, deadline);
        assert_eq!(retries.entries["peer"].reservation, None);
        assert!(!retries.ready(&"peer", &1, now + Duration::from_secs(10)));
        assert!(retries.ready(&"peer", &2, now));
        let replacement = retries.reserve("peer", 2, now).unwrap().0;
        retries.defer(&"peer", token, Duration::from_secs(300), now);
        assert_eq!(retries.entries["peer"].reservation, Some(replacement));
        assert_eq!(retries.entries["peer"].failures, 0);
        retries.clear();
        let fresh = retries.reserve("peer", 2, now).unwrap().0;
        retries.defer(&"peer", replacement, Duration::from_secs(300), now);
        assert!(retries.owns(&"peer", fresh));
    }

    #[test]
    fn local_deferral_has_a_finite_delay_without_creating_peer_failure_evidence() {
        let mut retries = RetrySchedule::default();
        let now = Instant::now();
        let token = retries.reserve("peer", 1, now).unwrap().0;
        let completed = now + Duration::from_secs(2);
        retries.defer(&"peer", token, Duration::from_secs(5), completed);
        assert_eq!(retries.entries["peer"].failures, 0);
        assert!(!retries.ready(&"peer", &1, completed + Duration::from_secs(4)));
        assert!(retries.ready(&"peer", &1, completed + Duration::from_secs(5)));
        retries.defer(&"peer", token, Duration::from_secs(300), completed);
        assert!(retries.ready(&"peer", &1, completed + Duration::from_secs(5)));
    }

    #[test]
    fn historical_failure_ceiling_is_isolated_from_ordinary_delivery() {
        let mut historical = RetrySchedule::with_failure_ceiling(Duration::from_secs(300));
        let mut ordinary = RetrySchedule::default();
        let mut now = Instant::now();
        for _ in 0..40 {
            historical.finished("peer", 1, false, now);
            ordinary.finished("peer", 1, false, now);
            now += Duration::from_secs(301);
        }
        historical.finished("peer", 1, false, now);
        ordinary.finished("peer", 1, false, now);
        assert!(!historical.ready(&"peer", &1, now + Duration::from_secs(299)));
        assert!(historical.ready(&"peer", &1, now + Duration::from_secs(300)));
        assert!(ordinary.ready(&"peer", &1, now + Duration::from_secs(15)));
        assert!(historical.ready(&"peer", &2, now));
        let token = historical.reserve("peer", 2, now).unwrap().0;
        historical.complete(&"peer", token, true, now);
        assert!(historical.ready(&"peer", &2, now));
    }

    #[test]
    fn reservation_survives_cancellation_and_completion_sets_single_failure_deadline() {
        let mut retries = RetrySchedule::default();
        let now = Instant::now();
        let token = retries.reserve("file", 1, now).unwrap().0;
        assert!(!retries.ready(&"file", &1, now + Duration::from_millis(900)));
        assert_eq!(retries.entries["file"].failures, 0);
        let completed = now + Duration::from_secs(20);
        retries.complete(&"file", token, false, completed);
        assert_eq!(retries.entries["file"].failures, 1);
        retries.complete(&"file", token, false, completed + Duration::from_secs(1));
        assert_eq!(retries.entries["file"].failures, 1);
        assert!(!retries.ready(&"file", &1, completed + Duration::from_millis(900)));
        assert!(retries.ready(&"file", &1, completed + Duration::from_millis(1250)));
    }

    #[test]
    fn clean_boundaries_clear_reservations_without_delaying_same_phase_work() {
        let mut retries = RetrySchedule::default();
        let now = Instant::now();
        for _ in 0..20 {
            let token = retries.reserve("chunks", 1, now).unwrap().0;
            retries.complete(&"chunks", token, true, now);
            assert!(retries.ready(&"chunks", &1, now));
            assert!(retries.entries.is_empty());
        }
    }

    #[test]
    fn replaced_routes_and_clear_reject_old_completion_tokens() {
        let mut retries = RetrySchedule::default();
        let now = Instant::now();
        let first = retries.reserve("file", 1, now).unwrap().0;
        let replacement = retries.reserve("file", 2, now).unwrap().0;
        retries.complete(&"file", first, false, now);
        assert_eq!(retries.entries["file"].reservation, Some(replacement));
        assert_eq!(retries.entries["file"].failures, 0);
        retries.clear();
        let fresh = retries.reserve("file", 1, now).unwrap().0;
        assert_ne!(first, fresh);
        retries.complete(&"file", first, true, now);
        assert_eq!(retries.entries["file"].reservation, Some(fresh));
    }

    #[test]
    fn active_reservations_cannot_be_evicted_after_their_provisional_deadline() {
        let mut retries = RetrySchedule::default();
        let now = Instant::now();
        for key in 0..CAPACITY {
            assert!(retries.reserve(key, 1, now).is_some());
        }
        assert!(
            retries
                .reserve(CAPACITY, 1, now + Duration::from_secs(20))
                .is_none()
        );
        assert_eq!(retries.entries.len(), CAPACITY);
    }

    #[test]
    fn reservation_inventory_defers_churn_without_evicting_unexpired_work() {
        let mut retries = RetrySchedule::default();
        let now = Instant::now();
        for key in 0..CAPACITY {
            let token = retries.reserve(key, 1, now).unwrap().0;
            if key % 2 == 0 {
                retries.complete(&key, token, false, now);
            }
        }
        for key in CAPACITY..CAPACITY * 2 {
            assert!(retries.reserve(key, 1, now).is_none());
        }
        assert_eq!(retries.entries.len(), CAPACITY);
        for key in 0..CAPACITY {
            assert!(!retries.ready(&key, &1, now));
        }
        assert!(
            retries
                .reserve(CAPACITY, 1, now + Duration::from_secs(2))
                .is_some()
        );
        assert_eq!(retries.entries.len(), CAPACITY);
    }

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
