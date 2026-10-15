//! Discover opt-in peers through an already discovered post office. All claims
//! remain self-signed, and the relay endpoint is kept separate from direct routes.

use super::*;
use crate::discovery::Announce;
use crate::discovery::roster::{PeerRecord, RouteSource, UpdateOutcome};
use crate::node::bridge_directory;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const BRIDGE_TIMEOUT: Duration = Duration::from_secs(10);

fn local_bridge_relay(
    roster: &crate::discovery::roster::Roster,
    excluded: Option<&str>,
) -> Option<PeerRecord> {
    roster
        .post_offices()
        .into_iter()
        .filter(|relay| {
            excluded.is_none_or(|id| relay.public.user_id() != id)
                && relay.routes.iter().any(|route| {
                    matches!(
                        route.source,
                        RouteSource::Lan | RouteSource::RoutedQuery | RouteSource::Authenticated
                    )
                })
        })
        .min_by_key(|relay| relay.public.user_id())
}

fn uses_gateway(relay: &PeerRecord) -> bool {
    relay
        .routes
        .iter()
        .any(|route| route.addr == relay.addr && route.source == RouteSource::Gateway)
}

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

    /// One bounded registration and directory refresh. It is safe to call on a
    /// timer; no VPN inventory or Tailscale dependency exists here.
    pub async fn refresh_bridge_directory(&self) -> usize {
        if !self.bridge_discovery_enabled.load(Ordering::Acquire)
            || self.privacy_snapshot().invisible
        {
            return 0;
        }
        let hub = self
            .bridge_hub_id
            .lock()
            .expect("bridge hub lock not poisoned")
            .clone();
        // A local site relay first introduces a configured hub. When the
        // chosen relay itself uses a gateway, refresh that local route too.
        let bootstrap = {
            let roster = self.roster.lock().expect("roster lock not poisoned");
            let chosen = self.selected_post_office(&roster);
            if chosen.as_ref().is_some_and(uses_gateway) || (hub.is_some() && chosen.is_none()) {
                let excluded = chosen
                    .as_ref()
                    .map(|relay| relay.public.user_id())
                    .or_else(|| hub.clone());
                local_bridge_relay(&roster, excluded.as_deref())
            } else {
                None
            }
        };
        if let Some(local) = bootstrap
            && let Some((proofs, gateway)) = self.query_bridge_relay(&local).await
            && self
                .bridge_hub_id
                .lock()
                .expect("bridge hub lock not poisoned")
                .as_deref()
                == hub.as_deref()
        {
            self.record_gateway(&local, &proofs, gateway);
        }
        let relay = {
            let roster = self.roster.lock().expect("roster lock not poisoned");
            self.selected_post_office(&roster)
        };
        let Some(relay) = relay else { return 0 };
        let Some((proofs, gateway)) = self.query_bridge_relay(&relay).await else {
            return 0;
        };
        // Recheck host policy after the network await; an invisible-mode switch
        // or relay preference change must not publish directory results.
        if !self.bridge_discovery_enabled.load(Ordering::Acquire)
            || self.privacy_snapshot().invisible
            || !self.relay_allowed(&relay.public)
            || *self
                .bridge_hub_id
                .lock()
                .expect("bridge hub lock not poisoned")
                != hub
        {
            return 0;
        }
        let now = Instant::now();
        let mut introduced = HashMap::new();
        {
            let mut roster = self.roster.lock().expect("roster lock not poisoned");
            for proof in proofs {
                if proof.public() == self.identity.public() {
                    continue;
                }
                if proof.post_office {
                    if proof.public() != relay.public
                        && let Some((ref id, port)) = gateway
                        && id == &proof.user_id
                    {
                        roster.update_gateway(&proof, relay.addr.ip(), port, &self.user_id());
                    }
                    continue;
                }
                if roster.update_relayed(&proof, relay.addr, &self.user_id())
                    != UpdateOutcome::Rejected
                {
                    introduced.insert(proof.user_id, now);
                }
            }
        }
        if self.cached_peer_snapshot_async().await.is_err() {
            let mut roster = self.roster.lock().expect("roster lock not poisoned");
            for id in introduced.keys() {
                roster.forget_relayed(id);
            }
            return 0;
        }
        // A completed refresh replaces the set. Peers missing from a response
        // cease to be authorized for automatic relay pulls immediately.
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
                != hub
        {
            drop(bridge_peers);
            let mut roster = self.roster.lock().expect("roster lock not poisoned");
            for id in introduced.keys() {
                roster.forget_relayed(id);
            }
            return 0;
        }
        let old = std::mem::replace(&mut *bridge_peers, introduced);
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
            let mut proofs = Vec::new();
            let mut gateway = None;
            for _ in 0..128 {
                let bytes = channel.recv().await.ok()?;
                let (page, route, last) = bridge_directory::decode_response_with_gateway(&bytes)
                    .or_else(|| {
                        bridge_directory::decode_response(&bytes).map(|(page, last)| {
                            let route = page
                                .iter()
                                .find(|proof| proof.post_office && proof.public() != relay.public)
                                .map(|proof| (proof.user_id.clone(), proof.tcp_port));
                            (page, route, last)
                        })
                    })?;
                if gateway.is_some() && route.is_some() && gateway != route {
                    return None;
                }
                gateway = route.or(gateway);
                proofs.extend(page);
                if proofs.len() > 128 {
                    return None;
                }
                if last {
                    return Some((proofs, gateway));
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
    use crate::identity::account::Account;
    use crate::identity::device::DeviceIdentity;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn gateway_election_keeps_a_local_site_relay_for_refresh() {
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
        assert_eq!(
            local_bridge_relay(&roster, Some(&hub.user_id()))
                .unwrap()
                .public,
            local.public()
        );
        assert!(
            elected
                .routes
                .iter()
                .any(|route| route.addr == elected.addr && route.source == RouteSource::Gateway)
        );
    }
}
