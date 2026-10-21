//! Bounded latency estimates for authenticated route attempts.

use std::{collections::HashMap, net::SocketAddr, time::Duration};

const MAX_ROUTES: usize = 256;
const MIN_DEADLINE: Duration = Duration::from_secs(3);
const MAX_DEADLINE: Duration = super::transport::HANDSHAKE_TIMEOUT;

struct Estimate {
    mean_ms: f64,
    variation_ms: f64,
    backoff: Duration,
    updated: u64,
}

/// Performance hints only: these estimates never authorize a route or identity.
#[derive(Default)]
pub(super) struct RouteTimings {
    entries: HashMap<(String, SocketAddr), Estimate>,
    sequence: u64,
}

impl RouteTimings {
    pub(super) fn deadline(&self, device: &str, address: SocketAddr) -> Duration {
        self.entries
            .get(&(device.to_owned(), address))
            .map_or(MAX_DEADLINE, |e| {
                Duration::from_secs_f64((e.mean_ms + 4.0 * e.variation_ms) / 1000.0)
                    .max(e.backoff)
                    .clamp(MIN_DEADLINE, MAX_DEADLINE)
            })
    }

    pub(super) fn succeeded(&mut self, device: &str, address: SocketAddr, elapsed: Duration) {
        let sample = elapsed.min(MAX_DEADLINE).as_secs_f64().max(0.001) * 1000.0;
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
        self.entries
            .entry(key)
            .and_modify(|e| {
                e.variation_ms = 0.75 * e.variation_ms + 0.25 * (sample - e.mean_ms).abs();
                e.mean_ms = 0.875 * e.mean_ms + 0.125 * sample;
                e.backoff = Duration::ZERO;
                e.updated = self.sequence;
            })
            .or_insert(Estimate {
                mean_ms: sample,
                variation_ms: sample / 2.0,
                backoff: Duration::ZERO,
                updated: self.sequence,
            });
    }

    pub(super) fn timed_out(&mut self, device: &str, address: SocketAddr) {
        let next = (self.deadline(device, address) * 2).min(MAX_DEADLINE);
        self.sequence = self.sequence.wrapping_add(1);
        if let Some(e) = self.entries.get_mut(&(device.to_owned(), address)) {
            e.backoff = next;
            e.updated = self.sequence;
        }
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
    fn unknown_routes_keep_the_existing_hard_ceiling() {
        let timings = RouteTimings::default();
        assert_eq!(timings.deadline("alice", route()), Duration::from_secs(10));
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
        assert_eq!(timings.deadline("alice", route()), Duration::from_secs(10));
        timings.succeeded("alice", route(), Duration::from_millis(100));
        assert_eq!(timings.deadline("alice", route()), Duration::from_secs(3));
    }
    #[test]
    fn slow_and_variable_routes_get_margin_but_do_not_change_other_devices_or_ports() {
        let mut timings = RouteTimings::default();
        timings.succeeded("alice", route(), Duration::from_secs(4));
        assert_eq!(timings.deadline("alice", route()), Duration::from_secs(10));
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
}
