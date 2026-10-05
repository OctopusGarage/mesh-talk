//! Routing hints are not presence. Only fresh discovery or pinned authentication
//! updates the live roster; expired/loaded hints remain private dial candidates.
use super::*;
use crate::discovery::roster::PeerRecord;
use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};

impl Node {
    /// Identity resolution has no endpoint requirement. Configured nodes use the
    /// durable signed binding; legacy SDK nodes retain only bounded memory history.
    pub(in crate::node) fn historical_peer_proofs(&self) -> Vec<crate::discovery::Announce> {
        let memory = self
            .roster
            .lock()
            .expect("roster lock not poisoned")
            .historical_announcements();
        let guard = self
            .privacy
            .state
            .read()
            .expect("privacy lock not poisoned");
        if let Some(state) = guard.as_ref() {
            let mut proofs = state.proofs.announcements();
            if !state.policy.snapshot().invisible {
                proofs.extend(memory.into_iter().filter(|proof| {
                    proof.account_id().is_none()
                        && state.proofs.by_author(&proof.ed25519_pub).is_none()
                }));
            }
            proofs
        } else {
            memory
        }
    }

    pub(in crate::node) fn historical_author(
        &self,
        author: &[u8; 32],
    ) -> Option<crate::discovery::Announce> {
        let proof = {
            let state = self
                .privacy
                .state
                .read()
                .expect("privacy lock not poisoned");
            if let Some(state) = state.as_ref() {
                state.proofs.by_author(author).or_else(|| {
                    if state.policy.snapshot().invisible {
                        return None;
                    }
                    self.roster
                        .lock()
                        .expect("roster lock not poisoned")
                        .historical_by_author(author)
                        .filter(|p| p.account_id().is_none())
                })
            } else {
                self.roster
                    .lock()
                    .expect("roster lock not poisoned")
                    .historical_by_author(author)
            }
        };
        proof.filter(|p| self.known_account_allowed(&p.public()))
    }

    pub(in crate::node) fn historical_dm_peers(&self) -> Vec<crate::discovery::Announce> {
        self.historical_peer_proofs()
            .into_iter()
            .filter(|p| {
                !p.post_office
                    && p.public() != self.identity.public()
                    && self.known_account_allowed(&p.public())
            })
            .collect()
    }

    pub(in crate::node) fn cached_routing_peers(&self) -> Vec<PeerRecord> {
        let guard = self
            .privacy
            .state
            .read()
            .expect("privacy lock not poisoned");
        let Some(state) = guard.as_ref() else {
            return Vec::new();
        };
        let candidates: Vec<_> = state
            .routes
            .routes()
            .into_iter()
            .filter_map(|(public, ip)| {
                let proof = state
                    .proofs
                    .by_author(&public.ed25519_pub)
                    .filter(|a| a.public() == public)?;
                let account = proof.account_id()?;
                let account_allowed = !state.policy.snapshot().invisible
                    || account == self.account_id()
                    || state.policy.allows(&account);
                Some((
                    account_allowed,
                    PeerRecord {
                        public,
                        addr: std::net::SocketAddr::new(ip, proof.tcp_port),
                        name: proof.name,
                        post_office: proof.post_office,
                        account_id: Some(account),
                        // Internal compatibility with send helpers, never inserted into
                        // the roster or returned by Runtime::peers as online presence.
                        last_seen: Instant::now() - Duration::from_secs(60),
                    },
                ))
            })
            .collect();
        // No policy/proof guard while consulting the joined-channel book. The
        // eventual pinned dial independently rechecks current admission.
        drop(guard);
        candidates
            .into_iter()
            .filter_map(|(account_allowed, peer)| {
                (account_allowed || self.group_member(&peer.public)).then_some(peer)
            })
            .collect()
    }

    pub(in crate::node) fn routing_peers(&self) -> Vec<PeerRecord> {
        let mut peers = self
            .roster
            .lock()
            .expect("roster lock not poisoned")
            .peers();
        let mut seen: HashSet<_> = peers.iter().map(|p| p.public.user_id()).collect();
        peers.extend(
            self.cached_routing_peers()
                .into_iter()
                .filter(|p| seen.insert(p.public.user_id())),
        );
        peers
    }

    pub(in crate::node) fn routing_peer(&self, user_id: &str) -> Option<PeerRecord> {
        self.routing_peers()
            .into_iter()
            .find(|p| p.public.user_id() == user_id)
    }

    pub(in crate::node) async fn probe_private_routes(self: &Arc<Self>) {
        self.probe_private_routes_with_budget(super::transport::HANDSHAKE_TIMEOUT)
            .await;
    }

    pub(in crate::node) async fn probe_private_routes_with_budget(
        self: &Arc<Self>,
        budget: Duration,
    ) {
        if self.cached_peer_snapshot_async().await.is_err() {
            log::warn!("verified discovery cache update failed");
        }
        let mut peers = self.cached_routing_peers();
        if peers.is_empty() {
            return;
        }
        // Advance a bounded batch before awaiting. Even if all eight selected
        // peers stall until the budget expires, later routes get the next turn.
        let start = self
            .privacy
            .route_probe_cursor
            .fetch_add(8, std::sync::atomic::Ordering::Relaxed) as usize
            % peers.len();
        peers.rotate_left(start);
        peers.truncate(8);
        // The selected batch is already bounded to eight. Admit each complete
        // child before spawning: authentication may synchronously persist proof
        // and route state after the parent has requested child cancellation.
        let mut probes = tokio::task::JoinSet::new();
        for peer in peers {
            let Some(work) = self.runtime_work.admit() else {
                return;
            };
            let node = self.clone();
            probes.spawn(work.track(async move {
                let _ = node.privacy_dial(peer.addr, &peer.public).await;
            }));
        }
        let _ = tokio::time::timeout(budget, async {
            while probes.join_next().await.is_some() {}
        })
        .await;
    }
}
