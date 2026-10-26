//! Bounded latency estimates for authenticated route attempts.

use std::{
    collections::HashMap,
    net::SocketAddr,
    time::{Duration, Instant},
};

const MAX_ROUTES: usize = 256;
const MIN_DEADLINE: Duration = Duration::from_secs(3);
const COLD_DEADLINE: Duration = Duration::from_secs(10);
pub(super) const MAX_AUTHENTICATION_DEADLINE: Duration = Duration::from_secs(20);
const MAX_DEADLINE: Duration = MAX_AUTHENTICATION_DEADLINE;
const MIN_CONTROL_DEADLINE: Duration = Duration::from_secs(15);
pub(super) const MAX_CONTROL_DEADLINE: Duration = Duration::from_secs(60);

struct Estimate {
    observed_at: Instant,
    authentication: Option<Sample>,
    control: Option<Sample>,
    backoff: Duration,
    control_backoff: Duration,
    updated: u64,
    authentication_updated: u64,
    control_updated: u64,
}

struct Sample {
    mean_ms: f64,
    variation_ms: f64,
}

impl Sample {
    fn observe(slot: &mut Option<Self>, elapsed: Duration) {
        let sample = elapsed.as_secs_f64().max(0.001) * 1000.0;
        if let Some(e) = slot {
            e.variation_ms = 0.75 * e.variation_ms + 0.25 * (sample - e.mean_ms).abs();
            e.mean_ms = 0.875 * e.mean_ms + 0.125 * sample;
        } else {
            *slot = Some(Self {
                mean_ms: sample,
                variation_ms: sample / 2.0,
            });
        }
    }

    fn margin(&self) -> Duration {
        Duration::from_secs_f64((self.mean_ms + 4.0 * self.variation_ms) / 1000.0)
    }
}

/// Performance hints only: these estimates never authorize a route or identity.
#[derive(Default)]
pub(super) struct RouteTimings {
    entries: HashMap<(String, SocketAddr), Estimate>,
    sequence: u64,
}

impl RouteTimings {
    /// Unclamped measured cost, separate from conservative protocol deadlines.
    /// Expired hints cannot pin an automatic route after network conditions change.
    pub(super) fn routing_cost(
        &self,
        device: &str,
        address: SocketAddr,
        now: Instant,
    ) -> Option<Duration> {
        let e = self.entries.get(&(device.to_owned(), address))?;
        if now.saturating_duration_since(e.observed_at) >= Duration::from_secs(120) {
            return None;
        }
        let measured = e
            .control
            .as_ref()
            .map(Sample::margin)
            .or_else(|| e.authentication.as_ref().map(|sample| sample.margin() * 3));
        measured.map(|cost| cost.max(e.backoff * 3).max(e.control_backoff))
    }

    pub(super) fn control_timed_out(&mut self, device: &str, address: SocketAddr) {
        let next = (self.control_deadline(device, address) * 2).min(MAX_CONTROL_DEADLINE);
        self.entry(device, address).control_backoff = next;
    }
    pub(super) fn deadline(&self, device: &str, address: SocketAddr) -> Duration {
        self.entries
            .get(&(device.to_owned(), address))
            .map_or(COLD_DEADLINE, |e| {
                e.authentication
                    .as_ref()
                    .map_or(COLD_DEADLINE, Sample::margin)
                    .max(e.backoff)
                    .clamp(MIN_DEADLINE, MAX_DEADLINE)
            })
    }

    pub(super) fn succeeded(&mut self, device: &str, address: SocketAddr, elapsed: Duration) {
        let e = self.entry(device, address);
        Sample::observe(&mut e.authentication, elapsed.min(MAX_DEADLINE));
        e.authentication_updated = e.updated;
        e.backoff = Duration::ZERO;
    }

    pub(super) fn control_deadline(&self, device: &str, address: SocketAddr) -> Duration {
        self.entries
            .get(&(device.to_owned(), address))
            .map_or(MIN_CONTROL_DEADLINE, |e| {
                // An exchange includes authentication and multiple sync/control round trips.
                let authentication = if e.authentication_updated > e.control_updated {
                    e.authentication
                        .as_ref()
                        .map_or(Duration::ZERO, Sample::margin)
                        * 3
                } else {
                    Duration::ZERO
                };
                let control = e.control.as_ref().map_or(Duration::ZERO, Sample::margin);
                authentication
                    .max(e.backoff * 3)
                    .max(control * 2)
                    .max(e.control_backoff)
                    .clamp(MIN_CONTROL_DEADLINE, MAX_CONTROL_DEADLINE)
            })
    }

    pub(super) fn control_succeeded(
        &mut self,
        device: &str,
        address: SocketAddr,
        elapsed: Duration,
    ) {
        let e = self.entry(device, address);
        Sample::observe(&mut e.control, elapsed.min(MAX_CONTROL_DEADLINE));
        e.control_updated = e.updated;
        e.control_backoff = Duration::ZERO;
    }

    fn entry(&mut self, device: &str, address: SocketAddr) -> &mut Estimate {
        self.sequence = self.sequence.wrapping_add(1);
        let key = (device.to_owned(), address);
        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_ROUTES {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.updated)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            }
        }
        let e = self.entries.entry(key).or_insert(Estimate {
            observed_at: Instant::now(),
            authentication: None,
            control: None,
            backoff: Duration::ZERO,
            control_backoff: Duration::ZERO,
            updated: self.sequence,
            authentication_updated: 0,
            control_updated: 0,
        });
        e.observed_at = Instant::now();
        e.updated = self.sequence;
        e
    }

    pub(super) fn timed_out(&mut self, device: &str, address: SocketAddr) {
        let next = (self.deadline(device, address) * 2).min(MAX_DEADLINE);
        self.entry(device, address).backoff = next;
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn route() -> std::net::SocketAddr {
        "127.0.0.1:47479".parse().unwrap()
    }
    #[test]
    fn routing_cost_uses_unclamped_recent_samples_and_failure_penalties() {
        let mut timings = RouteTimings::default();
        let now = std::time::Instant::now();
        timings.control_succeeded("fast", route(), Duration::from_millis(20));
        timings.control_succeeded("slow", route(), Duration::from_millis(500));
        assert!(
            timings.routing_cost("fast", route(), now).unwrap()
                < timings.routing_cost("slow", route(), now).unwrap()
        );
        timings.control_timed_out("fast", route());
        assert!(
            timings.routing_cost("fast", route(), now).unwrap()
                > timings.routing_cost("slow", route(), now).unwrap()
        );
        assert!(
            timings
                .routing_cost("slow", route(), now + Duration::from_secs(121))
                .is_none()
        );
        timings.clear();
        assert!(timings.routing_cost("slow", route(), now).is_none());
    }

    #[test]
    fn stalled_control_exchange_backs_off_even_when_authentication_is_fast() {
        let mut timings = RouteTimings::default();
        timings.succeeded("alice", route(), Duration::from_millis(50));
        timings.control_timed_out("alice", route());
        assert_eq!(
            timings.control_deadline("alice", route()),
            Duration::from_secs(30)
        );
        timings.succeeded("alice", route(), Duration::from_millis(50));
        assert_eq!(
            timings.control_deadline("alice", route()),
            Duration::from_secs(30)
        );
        for _ in 0..10 {
            timings.control_timed_out("alice", route());
        }
        assert_eq!(
            timings.control_deadline("alice", route()),
            Duration::from_secs(60)
        );
        timings.control_succeeded("alice", route(), Duration::from_millis(50));
        assert_eq!(
            timings.control_deadline("alice", route()),
            Duration::from_secs(15)
        );
    }
    #[test]
    fn fast_reused_exchanges_can_recover_from_old_slow_handshake_measurements() {
        let mut timings = RouteTimings::default();
        timings.succeeded("alice", route(), Duration::from_secs(8));
        assert_eq!(
            timings.control_deadline("alice", route()),
            Duration::from_secs(60)
        );
        for _ in 0..100 {
            timings.control_succeeded("alice", route(), Duration::from_millis(100));
        }
        assert_eq!(
            timings.control_deadline("alice", route()),
            Duration::from_secs(15)
        );
        timings.succeeded("alice", route(), Duration::from_secs(8));
        assert_eq!(
            timings.control_deadline("alice", route()),
            Duration::from_secs(60),
            "a new slow authentication must inform its in-flight exchange"
        );
    }
    #[test]
    fn unknown_routes_start_with_ten_seconds() {
        let timings = RouteTimings::default();
        assert_eq!(timings.deadline("alice", route()), Duration::from_secs(10));
    }
    #[test]
    fn never_successful_route_backs_off_after_real_timeouts() {
        let mut timings = RouteTimings::default();
        timings.timed_out("alice", route());
        assert_eq!(timings.deadline("alice", route()), Duration::from_secs(20));
        assert_eq!(timings.deadline("bob", route()), Duration::from_secs(10));
        timings.succeeded("alice", route(), Duration::from_millis(100));
        assert_eq!(timings.deadline("alice", route()), Duration::from_secs(3));
    }
    #[test]
    fn fast_routes_back_off_after_timeout_without_unbounded_growth() {
        let mut timings = RouteTimings::default();
        timings.succeeded("alice", route(), Duration::from_millis(100));
        assert_eq!(timings.deadline("alice", route()), Duration::from_secs(3));
        timings.timed_out("alice", route());
        assert_eq!(timings.deadline("alice", route()), Duration::from_secs(6));
        for _ in 0..10 {
            timings.timed_out("alice", route());
        }
        assert_eq!(timings.deadline("alice", route()), Duration::from_secs(20));
        timings.succeeded("alice", route(), Duration::from_millis(100));
        assert_eq!(timings.deadline("alice", route()), Duration::from_secs(3));
    }
    #[test]
    fn slow_and_variable_routes_get_margin_but_do_not_change_other_devices_or_ports() {
        let mut timings = RouteTimings::default();
        timings.succeeded("alice", route(), Duration::from_secs(4));
        assert!(timings.deadline("alice", route()) > Duration::from_secs(10));
        timings.succeeded("bob", route(), Duration::from_millis(50));
        assert_eq!(timings.deadline("bob", route()), Duration::from_secs(3));
        let other = "127.0.0.1:47480".parse().unwrap();
        assert_eq!(timings.deadline("bob", other), Duration::from_secs(10));
        let before = timings.deadline("bob", route());
        timings.succeeded("bob", route(), Duration::from_secs(3));
        assert!(timings.deadline("bob", route()) > before);
    }
    #[test]
    fn network_change_and_capacity_eviction_discard_old_estimates() {
        let mut timings = RouteTimings::default();
        timings.succeeded("alice", route(), Duration::from_millis(100));
        timings.clear();
        assert_eq!(timings.deadline("alice", route()), Duration::from_secs(10));
        for i in 0..300 {
            timings.succeeded(&format!("peer-{i}"), route(), Duration::from_millis(100));
        }
        assert_eq!(timings.len(), 256);
        assert_eq!(timings.deadline("peer-0", route()), Duration::from_secs(10));
        assert_eq!(
            timings.deadline("peer-299", route()),
            Duration::from_secs(3)
        );
    }
    #[test]
    fn complete_exchange_budget_adapts_without_penalizing_another_peer() {
        let mut timings = RouteTimings::default();
        assert_eq!(
            timings.control_deadline("alice", route()),
            Duration::from_secs(15)
        );
        timings.succeeded("alice", route(), Duration::from_secs(4));
        assert!(timings.control_deadline("alice", route()) > Duration::from_secs(15));
        timings.control_succeeded("alice", route(), Duration::from_secs(18));
        assert!(timings.control_deadline("alice", route()) >= Duration::from_secs(18));
        assert_eq!(
            timings.control_deadline("bob", route()),
            Duration::from_secs(15)
        );
        for _ in 0..100 {
            timings.control_succeeded("alice", route(), Duration::from_millis(50));
        }
        timings.succeeded("alice", route(), Duration::from_millis(50));
        timings.timed_out("alice", route());
        assert!(timings.control_deadline("alice", route()) <= Duration::from_secs(60));
        timings.clear();
        assert_eq!(
            timings.control_deadline("alice", route()),
            Duration::from_secs(15)
        );
    }
}
