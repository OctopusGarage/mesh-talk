//! Routing hints are not presence. Only fresh discovery or pinned authentication
//! updates the live roster; expired/loaded hints remain private dial candidates.
use super::*;
use crate::discovery::roster::PeerRecord;
use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};

// Each historical probe can race two addresses. This producer alone uses at
// most four of the seven outbound slots; other producers share the remainder.
const MAX_ACTIVE_PRIVATE_PROBES: usize = 2;
const MAX_PRIVATE_PROBES_PER_ROUND: usize = 8;

type ProbeIdentity = ([u8; 32], [u8; 32]);
pub(in crate::node) type PrivateProbeRetries =
    super::delivery_retry::RetrySchedule<ProbeIdentity, (u64, u64, Vec<std::net::SocketAddr>)>;

impl Node {
    /// Infrastructure classification from a validated full device binding.
    pub fn device_is_post_office(&self, device: &crate::identity::device::PublicIdentity) -> bool {
        self.historical_author(&device.ed25519_pub)
            .is_some_and(|proof| proof.public() == *device && proof.post_office)
    }

    /// A verified account binding for a channel member, including members no longer in
    /// the live roster. Match the full device identity before trusting the stored proof.
    pub fn account_id_for_device(
        &self,
        device: &crate::identity::device::PublicIdentity,
    ) -> Option<String> {
        if *device == self.identity.public() || self.certified_sibling(device) {
            return Some(self.account_id());
        }
        self.historical_author(&device.ed25519_pub)
            .filter(|proof| proof.public() == *device)
            .and_then(|proof| proof.account_id())
    }
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
                        routes: Vec::new(),
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
                (account_allowed
                    || match self.group_member(&peer.public) {
                        Ok(member) => member,
                        Err(error) => {
                            log::warn!("routing candidate deferred after storage failure: {error}");
                            return None;
                        }
                    }
                    || self.group_departure_peer(&peer.public))
                .then_some(peer)
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

    pub(in crate::node) async fn run_private_route_probes(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut network = self.network_changes.subscribe();
        loop {
            // Schedule from the start of a round. Sleeping after a slow ten-
            // second handshake used to leave a twenty-second refresh gap for
            // fifteen-second public evidence, even on a reachable path.
            tokio::select! {
                _ = tick.tick() => {},
                _ = network.changed() => {},
            }
            self.probe_private_routes().await;
        }
    }

    pub(in crate::node) async fn probe_private_routes_with_budget(
        self: &Arc<Self>,
        budget: Duration,
    ) {
        if self.cached_peer_snapshot_async().await.is_err() {
            log::warn!("verified discovery cache update failed");
        }
        let mut peers = self.cached_routing_peers();
        // A dial already races this device's verified address candidates. Cache
        // entries are route hints, not independent probe destinations: spawning
        // once per address lets one moved device consume every outbound slot.
        let mut devices = HashSet::new();
        peers.retain(|peer| devices.insert(peer.public.user_id()));
        if peers.is_empty() {
            return;
        }
        // Advance only launched or deliberately deferred devices. Cancellation
        // must not skip the suffix that this round never examined.
        let start = self
            .privacy
            .route_probe_cursor
            .load(std::sync::atomic::Ordering::Relaxed) as usize
            % peers.len();
        peers.rotate_left(start);
        peers.truncate(MAX_PRIVATE_PROBES_PER_ROUND);
        let mut peers = peers.into_iter();
        let mut reserved = Vec::new();
        // Admit every complete child before spawning. The JoinSet remains owned
        // by this round, including when its unchanged whole-round budget expires.
        let mut probes = tokio::task::JoinSet::new();
        let _ = tokio::time::timeout(budget, async {
            loop {
                while probes.len() < MAX_ACTIVE_PRIVATE_PROBES {
                    let Some(peer) = peers.next() else {
                        break;
                    };
                    let key = (peer.public.ed25519_pub, peer.public.x25519_pub);
                    let announcement = self
                        .roster
                        .lock()
                        .expect("roster lock")
                        .announcement(&peer.public)
                        .cloned();
                    let mut routes = self.privacy_route_candidates(
                        peer.addr,
                        &peer.public,
                        announcement.as_ref(),
                    );
                    // Affinity ordering is not a changed route set. Ordinary
                    // heartbeats must not reset a failed historical probe.
                    routes.sort_unstable();
                    let generation = self
                        .privacy
                        .generation
                        .load(std::sync::atomic::Ordering::Acquire);
                    let network = *self.network_changes.borrow();
                    let reservation = self
                        .privacy
                        .route_probe_retries
                        .lock()
                        .expect("probe retry lock")
                        .reserve(key, (generation, network, routes), Instant::now());
                    let Some((token, _)) = reservation else {
                        // This device retains its retry deadline, so a deferred
                        // prefix must not hide later healthy devices each round.
                        self.privacy
                            .route_probe_cursor
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        continue;
                    };
                    let Some(work) = self.runtime_work.admit() else {
                        self.privacy
                            .route_probe_retries
                            .lock()
                            .expect("probe retry lock")
                            .forget(&key);
                        return;
                    };
                    self.privacy
                        .route_probe_cursor
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let node = self.clone();
                    reserved.push((key, token));
                    probes.spawn(work.track(async move {
                        let result = node.privacy_dial(peer.addr, &peer.public).await;
                        let clean = result.is_ok()
                            || result
                                .as_ref()
                                .is_err_and(super::session_pool::local_capacity_busy);
                        (key, token, clean)
                    }));
                }
                match probes.join_next().await {
                    Some(Ok((key, token, clean))) => {
                        reserved.retain(|entry| *entry != (key, token));
                        self.privacy
                            .route_probe_retries
                            .lock()
                            .expect("probe retry lock")
                            .complete(&key, token, clean, Instant::now());
                    }
                    Some(Err(_)) => {}
                    None => break,
                }
            }
        })
        .await;
        // A spent round budget is no progress too. These advisory hints never
        // grant presence or retire durable work. Dropping the JoinSet cancels
        // its actual socket owners; new/changed routes bypass the old deadline.
        for (key, token) in reserved {
            self.privacy
                .route_probe_retries
                .lock()
                .expect("probe retry lock")
                .complete(&key, token, false, Instant::now());
        }
    }
}
