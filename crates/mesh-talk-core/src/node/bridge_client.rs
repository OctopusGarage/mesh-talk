//! Discover opt-in peers through an already discovered post office. All claims
//! remain self-signed, and the relay endpoint is kept separate from direct routes.

use super::*;
use crate::discovery::roster::UpdateOutcome;
use crate::node::bridge_directory;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const BRIDGE_TIMEOUT: Duration = Duration::from_secs(10);

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
        let relay = {
            let roster = self.roster.lock().expect("roster lock not poisoned");
            super::postbox::elected_post_office(&roster)
        };
        let Some(relay) = relay else { return 0 };
        if !self.relay_allowed(&relay.public) {
            return 0;
        }
        let Ok(mut channel) = self.privacy_dial(relay.addr, &relay.public).await else {
            return 0;
        };
        let response = tokio::time::timeout(BRIDGE_TIMEOUT, async {
            channel.send(bridge_directory::request()).await?;
            let mut proofs = Vec::new();
            for _ in 0..128 {
                let bytes = channel.recv().await?;
                let (page, last) = bridge_directory::decode_response(&bytes)
                    .ok_or(crate::transport::TransportError::IdentityMismatch)?;
                proofs.extend(page);
                if proofs.len() > 128 {
                    return Err(crate::transport::TransportError::IdentityMismatch);
                }
                if last {
                    return Ok(proofs);
                }
            }
            Err(crate::transport::TransportError::IdentityMismatch)
        })
        .await;
        let Ok(Ok(proofs)) = response else { return 0 };
        // Recheck host policy after the network await; an invisible-mode switch
        // or relay preference change must not publish directory results.
        if !self.bridge_discovery_enabled.load(Ordering::Acquire)
            || self.privacy_snapshot().invisible
            || !self.relay_allowed(&relay.public)
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
                    if proof.public() != relay.public {
                        roster.update_gateway(&proof, relay.addr.ip(), &self.user_id());
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
}
