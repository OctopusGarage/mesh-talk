//! Discover opt-in peers through an already discovered post office. All claims
//! remain self-signed, and the relay endpoint is kept separate from direct routes.

use super::*;
use crate::discovery::Announce;
use crate::discovery::roster::{PeerRecord, UpdateOutcome};
use crate::node::bridge_directory;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const BRIDGE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_DIRECTORY_RELAYS_PER_REFRESH: usize = 8;
const DIRECTORY_MEMBERSHIP_TTL: Duration = Duration::from_secs(45);

impl Node {
    /// Explicitly share this device's public presence with a discovered relay
    /// directory. Disabling immediately revokes introduced-peer recovery.
    pub fn set_bridge_discovery_enabled(&self, enabled: bool) {
        self.bridge_discovery_enabled
            .store(enabled, Ordering::Release);
        if !enabled {
            self.clear_bridge_introductions();
        }
        self.delivery_notify.notify_one();
        if enabled {
            self.bridge_notify.notify_one();
        }
    }

    pub(in crate::node) fn clear_bridge_introductions(&self) {
        let introduced = std::mem::take(
            &mut *self
                .bridge_dm_pull_peers
                .lock()
                .expect("bridge peers lock not poisoned"),
        );
        let mut roster = self.roster.lock().expect("roster lock not poisoned");
        for id in introduced.keys() {
            roster.forget_relayed(id);
        }
    }

    /// Register with each reachable, verified post office. Local and gateway
    /// relays are peers: clients use their local relay first, while registrations
    /// on both sides make cross-site introductions and mailbox drains possible.
    pub async fn refresh_bridge_directory(&self) -> usize {
        if !self.bridge_discovery_enabled.load(Ordering::Acquire)
            || self.privacy_snapshot().invisible
        {
            return 0;
        }
        let preference = self
            .bridge_hub_id
            .lock()
            .expect("bridge hub lock not poisoned")
            .clone();
        let candidates = {
            let roster = self.roster.lock().expect("roster lock not poisoned");
            self.post_office_candidates(&roster)
        };
        let mut introduced = HashMap::new();
        let mut successful = 0;
        let responses = futures_util::future::join_all(
            candidates
                .into_iter()
                .take(MAX_DIRECTORY_RELAYS_PER_REFRESH)
                .map(|relay| async {
                    let response = self.query_bridge_relay(&relay).await;
                    (relay, response)
                }),
        )
        .await;
        for (relay, response) in responses {
            let Some((proofs, gateway)) = response else {
                continue;
            };
            // A policy change during an awaited query must not publish its results.
            if !self.bridge_discovery_enabled.load(Ordering::Acquire)
                || self.privacy_snapshot().invisible
                || !self.relay_allowed(&relay.public)
                || *self
                    .bridge_hub_id
                    .lock()
                    .expect("bridge hub lock not poisoned")
                    != preference
            {
                return 0;
            }
            successful += 1;
            self.record_gateway(&relay, &proofs, gateway);
            let mut roster = self.roster.lock().expect("roster lock not poisoned");
            for proof in proofs {
                if proof.public() == self.identity.public() {
                    continue;
                }
                if proof.post_office {
                    continue;
                }
                if roster.update_relayed(&proof, relay.addr, &self.user_id())
                    != UpdateOutcome::Rejected
                {
                    introduced.insert(proof.user_id, Instant::now());
                }
            }
        }
        if successful == 0 {
            return 0;
        }
        if self.cached_peer_snapshot_async().await.is_err() {
            let mut roster = self.roster.lock().expect("roster lock not poisoned");
            for id in introduced.keys() {
                roster.forget_relayed(id);
            }
            return 0;
        }
        // A directory can be temporarily unreachable while another succeeds.
        // Keep its last verified introductions only until the membership TTL.
        let count = introduced.len();
        let mut bridge_peers = self
            .bridge_dm_pull_peers
            .lock()
            .expect("bridge peers lock not poisoned");
        if !self.bridge_discovery_enabled.load(Ordering::Acquire)
            || self.privacy_snapshot().invisible
            || *self
                .bridge_hub_id
                .lock()
                .expect("bridge hub lock not poisoned")
                != preference
        {
            drop(bridge_peers);
            let mut roster = self.roster.lock().expect("roster lock not poisoned");
            for id in introduced.keys() {
                roster.forget_relayed(id);
            }
            return 0;
        }
        let old = std::mem::take(&mut *bridge_peers);
        let now = Instant::now();
        for (id, seen) in old.iter() {
            if now.saturating_duration_since(*seen) < DIRECTORY_MEMBERSHIP_TTL {
                introduced.entry(id.clone()).or_insert(*seen);
            }
        }
        *bridge_peers = introduced;
        let current = bridge_peers.clone();
        drop(bridge_peers);
        let mut roster = self.roster.lock().expect("roster lock not poisoned");
        for id in old.keys().filter(|id| !current.contains_key(*id)) {
            roster.forget_relayed(id);
        }
        self.delivery_notify.notify_one();
        count
    }

    async fn query_bridge_relay(
        &self,
        relay: &PeerRecord,
    ) -> Option<(Vec<Announce>, Option<(String, u16)>)> {
        if !self.relay_allowed(&relay.public) {
            return None;
        }
        let response = if let Some(response) = self
            .query_bridge_relay_once(relay, bridge_directory::request())
            .await
        {
            response
        } else {
            self.query_bridge_relay_once(relay, bridge_directory::legacy_request())
                .await?
        };
        if !self.bridge_discovery_enabled.load(Ordering::Acquire)
            || self.privacy_snapshot().invisible
            || !self.relay_allowed(&relay.public)
        {
            return None;
        }
        Some(response)
    }

    async fn query_bridge_relay_once(
        &self,
        relay: &PeerRecord,
        request: &[u8],
    ) -> Option<(Vec<Announce>, Option<(String, u16)>)> {
        let mut channel = self.privacy_dial(relay.addr, &relay.public).await.ok()?;
        let response = tokio::time::timeout(BRIDGE_TIMEOUT, async {
            channel.send(request).await.ok()?;
            let mut response = bridge_directory::DirectoryResponse::new(relay.public.clone());
            for _ in 0..128 {
                let bytes = channel.recv().await.ok()?;
                match response.add_page(&bytes)? {
                    bridge_directory::DirectoryProgress::More => {}
                    bridge_directory::DirectoryProgress::Complete(snapshot) => {
                        return Some(snapshot);
                    }
                }
            }
            None
        })
        .await
        .ok()??;
        Some(response)
    }

    fn record_gateway(
        &self,
        relay: &PeerRecord,
        proofs: &[Announce],
        gateway: Option<(String, u16)>,
    ) {
        let Some((id, port)) = gateway else { return };
        let mut roster = self.roster.lock().expect("roster lock not poisoned");
        for proof in proofs {
            if proof.post_office && proof.user_id == id {
                roster.update_gateway(proof, relay.addr.ip(), port, &self.user_id());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::Roster;
    use crate::discovery::roster::RouteSource;
    use crate::identity::account::Account;
    use crate::identity::device::DeviceIdentity;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn gateway_directory_route_keeps_a_local_site_relay_available() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let (hub, local) = if a.user_id() < b.user_id() {
            (&a, &b)
        } else {
            (&b, &a)
        };
        let account = Account::generate();
        let mut roster = Roster::default();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        roster.update(
            &Announce::new_post_office_with_account(local, &account, "local", 47475),
            ip,
            "self",
        );
        roster.update_gateway(
            &Announce::new_post_office_with_account(hub, &account, "hub", 47476),
            ip,
            50000,
            "self",
        );
        let elected = super::super::postbox::elected_post_office(&roster).unwrap();
        assert_eq!(elected.public, hub.public());
        let candidates = super::super::postbox::post_office_candidates(&roster, None);
        assert_eq!(candidates[0].public, local.public());
        assert_eq!(candidates[1].public, hub.public());
        assert!(
            elected
                .routes
                .iter()
                .any(|route| route.addr == elected.addr && route.source == RouteSource::Gateway)
        );
    }
}
