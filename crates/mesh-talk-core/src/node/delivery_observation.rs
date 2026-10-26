//! Bounded runtime facts about admitted outgoing recipient work.

use super::{
    Node,
    delivery_store::{DeliveryDestination, DeliveryStatus, DeliveryStore},
};
use crate::eventlog::EventId;
use serde::Serialize;
use std::{collections::BTreeMap, time::Instant};

const CAPACITY: usize = 256;

/// A local scheduling fact, never transport success or recipient confirmation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryObservationKind {
    Attempting,
    RetryScheduled,
}

/// Locally evidenced recovery context, never a claim about remote presence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryRecoveryReason {
    PostOfficeDisabled,
    NoKnownRoute,
    AttemptTimedOut,
}

/// Runtime-only elapsed age; absent after eviction, cancellation or restart.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct DeliveryObservation {
    pub kind: DeliveryObservationKind,
    pub age_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<DeliveryRecoveryReason>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct AttemptToken {
    pub(super) logical: EventId,
    pub(super) destination: EventId,
    generation: u64,
    serial: u64,
}

struct Entry {
    token: AttemptToken,
    kind: DeliveryObservationKind,
    observed: Instant,
    reason: Option<DeliveryRecoveryReason>,
    touched: u64,
}

#[derive(Default)]
pub(super) struct DeliveryObservations {
    entries: BTreeMap<EventId, Entry>,
    generation: u64,
    serial: u64,
}

impl DeliveryObservations {
    pub(super) fn sync_generation(&mut self, generation: u64) {
        if self.generation != generation {
            self.clear();
            self.generation = generation;
        }
    }

    pub(super) fn begin(
        &mut self,
        logical: EventId,
        destination: EventId,
        generation: u64,
        now: Instant,
    ) -> AttemptToken {
        self.sync_generation(generation);
        self.serial = self.serial.checked_add(1).unwrap_or_else(|| {
            self.clear();
            1
        });
        let token = AttemptToken {
            logical,
            destination,
            generation,
            serial: self.serial,
        };
        if !self.entries.contains_key(&logical) && self.entries.len() >= CAPACITY {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, e)| (e.observed, e.touched))
                .map(|(id, _)| *id)
                .unwrap();
            self.entries.remove(&oldest);
        }
        self.entries.insert(
            logical,
            Entry {
                token,
                kind: DeliveryObservationKind::Attempting,
                observed: now,
                reason: None,
                touched: self.serial,
            },
        );
        token
    }

    #[cfg(test)]
    pub(super) fn finished(&mut self, token: AttemptToken, generation: u64, now: Instant) -> bool {
        self.finished_with_reason(token, generation, now, None)
    }

    pub(super) fn finished_with_reason(
        &mut self,
        token: AttemptToken,
        generation: u64,
        now: Instant,
        reason: Option<DeliveryRecoveryReason>,
    ) -> bool {
        self.sync_generation(generation);
        let next = self.serial.checked_add(1).unwrap_or_else(|| {
            self.clear();
            1
        });
        self.serial = next;
        if let Some(entry) = self
            .entries
            .get_mut(&token.logical)
            .filter(|e| e.token == token && token.generation == generation)
        {
            entry.kind = DeliveryObservationKind::RetryScheduled;
            entry.reason = reason;
            entry.observed = now;
            entry.touched = next;
            return true;
        }
        false
    }

    pub(super) fn snapshot(
        &mut self,
        logical: EventId,
        generation: u64,
        now: Instant,
    ) -> Option<DeliveryObservation> {
        self.sync_generation(generation);
        self.entries.get(&logical).map(|entry| DeliveryObservation {
            kind: entry.kind,
            reason: entry.reason,
            age_ms: now
                .saturating_duration_since(entry.observed)
                .as_millis()
                .min(u64::MAX as u128) as u64,
        })
    }

    pub(super) fn cancel(&mut self, token: AttemptToken) {
        if self
            .entries
            .get(&token.logical)
            .is_some_and(|e| e.token == token)
        {
            self.entries.remove(&token.logical);
        }
    }

    pub(super) fn retain(&mut self, mut eligible: impl FnMut(EventId, EventId) -> bool) {
        self.entries
            .retain(|id, entry| eligible(*id, entry.token.destination));
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
}

impl Node {
    pub(in crate::node) fn clear_delivery_observations(&self) {
        self.delivery_observations
            .lock()
            .expect("delivery observations lock not poisoned")
            .clear();
        self.delivery_status_notify.notify_waiters();
    }

    fn observed_destination_current(
        &self,
        store: &DeliveryStore,
        logical: EventId,
        event: EventId,
    ) -> bool {
        let Some(message) = store.message(logical) else {
            return false;
        };
        let Some(account) = message
            .recipient_account
            .as_deref()
            .filter(|a| *a != self.account_id())
        else {
            return false;
        };
        store.status_in(
            super::conversation::account_conversation_id(&self.account_id(), account),
            logical,
        ) == Some(DeliveryStatus::Awaiting)
            && message.destinations.iter().any(|destination| {
                destination.event.id == event
                    && destination.receipt_eligible
                    && destination.account.as_deref() == Some(account)
                    && self.delivery_destination_allowed(destination)
            })
    }

    pub(super) fn begin_delivery_observation(
        &self,
        logical: EventId,
        destination: &DeliveryDestination,
    ) -> Option<AttemptToken> {
        let store = self.delivery.lock().expect("delivery lock not poisoned");
        if !self.observed_destination_current(&store, logical, destination.event.id) {
            return None;
        }
        let mut observations = self
            .delivery_observations
            .lock()
            .expect("delivery observations lock not poisoned");
        if self.runtime_work.is_closed() {
            return None;
        }
        let generation = self
            .privacy
            .generation
            .load(std::sync::atomic::Ordering::SeqCst);
        let token = observations.begin(
            logical,
            destination.event.id,
            generation,
            std::time::Instant::now(),
        );
        self.delivery_status_notify.notify_waiters();
        Some(token)
    }

    #[cfg(test)]
    pub(super) fn finish_delivery_observation(&self, token: AttemptToken) {
        self.finish_delivery_observation_with_reason(token, None);
    }

    pub(super) fn finish_delivery_observation_with_reason(
        &self,
        token: AttemptToken,
        reason: Option<DeliveryRecoveryReason>,
    ) {
        let store = self.delivery.lock().expect("delivery lock not poisoned");
        let mut observations = self
            .delivery_observations
            .lock()
            .expect("delivery observations lock not poisoned");
        if self.runtime_work.is_closed()
            || !self.observed_destination_current(&store, token.logical, token.destination)
        {
            observations.cancel(token);
            return;
        }
        let generation = self
            .privacy
            .generation
            .load(std::sync::atomic::Ordering::SeqCst);
        if observations.finished_with_reason(token, generation, std::time::Instant::now(), reason) {
            self.delivery_status_notify.notify_waiters();
        }
    }

    pub(in crate::node) fn prune_delivery_observations(&self) {
        let store = self.delivery.lock().expect("delivery lock not poisoned");
        let mut observations = self
            .delivery_observations
            .lock()
            .expect("delivery observations lock not poisoned");
        observations.sync_generation(
            self.privacy
                .generation
                .load(std::sync::atomic::Ordering::SeqCst),
        );
        observations
            .retain(|logical, event| self.observed_destination_current(&store, logical, event));
    }

    /// Additive, bounded projection of local scheduling facts, authorized by the
    /// exact account conversation before any hint lookup. Missing means unknown.
    /// See docs/network-design-reference.md#runtime-delivery-observations.
    pub fn account_delivery_observations(
        &self,
        peer: &str,
        ids: &[EventId],
    ) -> Vec<(EventId, DeliveryStatus, Option<DeliveryObservation>)> {
        if peer == self.account_id() {
            return Vec::new();
        }
        let store = self.delivery.lock().expect("delivery lock not poisoned");
        let conv = super::conversation::account_conversation_id(&self.account_id(), peer);
        let mut observations = self
            .delivery_observations
            .lock()
            .expect("delivery observations lock not poisoned");
        let generation = self
            .privacy
            .generation
            .load(std::sync::atomic::Ordering::SeqCst);
        observations.sync_generation(generation);
        observations
            .retain(|logical, event| self.observed_destination_current(&store, logical, event));
        if self.runtime_work.is_closed() {
            observations.clear();
        }
        let now = std::time::Instant::now();
        ids.iter()
            .take(256)
            .filter_map(|id| {
                let status = store.status_in(conv, *id)?;
                let observation = (status == DeliveryStatus::Awaiting)
                    .then(|| observations.snapshot(*id, generation, now))
                    .flatten();
                Some((*id, status, observation))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn id(n: u16) -> crate::eventlog::EventId {
        let mut bytes = [0; 32];
        bytes[..2].copy_from_slice(&n.to_be_bytes());
        crate::eventlog::EventId::new(bytes)
    }

    #[test]
    fn recovery_reason_is_exact_attempt_scoped_and_optional() {
        let mut observations = DeliveryObservations::default();
        let now = Instant::now();
        let first = observations.begin(id(1), id(2), 0, now);
        observations.finished_with_reason(
            first,
            0,
            now,
            Some(DeliveryRecoveryReason::NoKnownRoute),
        );
        assert_eq!(
            observations.snapshot(id(1), 0, now).unwrap().reason,
            Some(DeliveryRecoveryReason::NoKnownRoute)
        );
        let current = observations.begin(id(1), id(2), 0, now);
        assert_eq!(observations.snapshot(id(1), 0, now).unwrap().reason, None);
        observations.finished_with_reason(
            first,
            0,
            now,
            Some(DeliveryRecoveryReason::AttemptTimedOut),
        );
        assert_eq!(observations.snapshot(id(1), 0, now).unwrap().reason, None);
        observations.finished_with_reason(
            current,
            1,
            now,
            Some(DeliveryRecoveryReason::PostOfficeDisabled),
        );
        assert!(observations.snapshot(id(1), 1, now).is_none());
    }

    #[test]
    fn missing_is_unknown_and_age_uses_monotonic_elapsed_time() {
        let mut observations = DeliveryObservations::default();
        let now = Instant::now();
        assert!(observations.snapshot(id(1), 0, now).is_none());
        let token = observations.begin(id(1), id(2), 0, now);
        let snapshot = observations
            .snapshot(id(1), 0, now + Duration::from_millis(123))
            .unwrap();
        assert_eq!(snapshot.kind, DeliveryObservationKind::Attempting);
        assert_eq!(snapshot.age_ms, 123);
        observations.finished(token, 0, now + Duration::from_secs(1));
        assert_eq!(
            observations
                .snapshot(id(1), 0, now + Duration::from_secs(2))
                .unwrap()
                .kind,
            DeliveryObservationKind::RetryScheduled
        );
    }

    #[test]
    fn cap_counts_logical_messages_and_ties_evict_in_admission_order() {
        let mut observations = DeliveryObservations::default();
        let now = Instant::now();
        for n in 0..256 {
            observations.begin(id(n), id(n + 1), 0, now);
        }
        observations.begin(id(1), id(999), 0, now);
        observations.begin(id(256), id(257), 0, now);
        assert_eq!(observations.entries.len(), 256);
        assert!(observations.snapshot(id(0), 0, now).is_none());
        assert!(observations.snapshot(id(1), 0, now).is_some());
    }

    #[test]
    fn stale_attempt_generation_and_evicted_completion_cannot_restore_a_hint() {
        let mut observations = DeliveryObservations::default();
        let now = Instant::now();
        let old = observations.begin(id(1), id(2), 0, now);
        let current = observations.begin(id(1), id(3), 0, now);
        observations.finished(old, 0, now);
        assert_eq!(
            observations.snapshot(id(1), 0, now).unwrap().kind,
            DeliveryObservationKind::Attempting
        );
        observations.finished(current, 1, now);
        assert!(observations.snapshot(id(1), 1, now).is_none());
        let token = observations.begin(id(1), id(2), 1, now);
        observations.clear();
        observations.finished(token, 1, now);
        assert!(observations.snapshot(id(1), 1, now).is_none());
    }

    #[test]
    fn cancel_only_removes_its_exact_current_attempt() {
        let mut observations = DeliveryObservations::default();
        let now = Instant::now();
        let old = observations.begin(id(1), id(2), 0, now);
        let current = observations.begin(id(1), id(3), 0, now);
        observations.cancel(old);
        assert!(observations.snapshot(id(1), 0, now).is_some());
        observations.cancel(current);
        assert!(observations.snapshot(id(1), 0, now).is_none());
    }
    #[test]
    fn completion_refreshes_eviction_order_without_allowing_evicted_tokens_to_reappear() {
        let mut observations = DeliveryObservations::default();
        let now = Instant::now();
        let oldest = observations.begin(id(0), id(500), 0, now);
        let evicted = observations.begin(id(1), id(501), 0, now);
        for n in 2..256 {
            observations.begin(id(n), id(n + 500), 0, now);
        }
        observations.finished(oldest, 0, now);
        observations.begin(id(256), id(756), 0, now);
        assert!(observations.snapshot(id(0), 0, now).is_some());
        assert!(observations.snapshot(id(1), 0, now).is_none());
        observations.begin(id(1), id(900), 0, now);
        observations.finished(evicted, 0, now);
        assert_eq!(
            observations.snapshot(id(1), 0, now).unwrap().kind,
            DeliveryObservationKind::Attempting
        );
    }
}
