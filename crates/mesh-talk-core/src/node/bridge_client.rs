//! Discover opt-in peers through an already discovered post office. All claims
//! remain self-signed, and the relay endpoint is kept separate from direct routes.

use super::*;
use crate::discovery::Announce;
use crate::discovery::roster::{PeerRecord, RelayedRollback};
use crate::node::bridge_directory;
use futures_util::{StreamExt, stream};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, atomic::Ordering};
use std::time::{Duration, Instant};

type BridgeResponse = (
    Vec<Announce>,
    Option<(String, u16)>,
    HashMap<String, bridge_directory::PresenceDeadline>,
);

const DIRECTORY_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
#[cfg(test)]
const BRIDGE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CONCURRENT_DIRECTORY_QUERIES: usize = 8;
const DIRECTORY_MEMBERSHIP_TTL: Duration = crate::discovery::roster::PUBLIC_PRESENCE_TTL;

#[derive(Default)]
pub(in crate::node) struct BridgeRefreshState {
    generation: Option<(u64, u64)>,
    relays: HashMap<([u8; 32], [u8; 32]), RelayQueryState>,
    active: Arc<Mutex<HashSet<DirectoryQueryKey>>>,
}

type DirectoryQueryKey = ([u8; 32], [u8; 32], std::net::SocketAddr);

/// Owns one exact relay query across overlapping refresh waves, including cancellation.
struct ActiveDirectoryQuery {
    active: Arc<Mutex<HashSet<DirectoryQueryKey>>>,
    key: DirectoryQueryKey,
}

impl ActiveDirectoryQuery {
    fn acquire(
        active: Arc<Mutex<HashSet<DirectoryQueryKey>>>,
        key: DirectoryQueryKey,
    ) -> Option<Self> {
        {
            let mut entries = active.lock().expect("directory activity lock not poisoned");
            if entries.len() >= MAX_CONCURRENT_DIRECTORY_QUERIES || !entries.insert(key) {
                return None;
            }
        }
        Some(Self { active, key })
    }
}

impl Drop for ActiveDirectoryQuery {
    fn drop(&mut self) {
        self.active
            .lock()
            .expect("directory activity lock not poisoned")
            .remove(&self.key);
    }
}

struct RelayQueryState {
    endpoint: std::net::SocketAddr,
    failures: u32,
    retry_at: Instant,
    version: usize,
    upgrade_at: Instant,
}

#[derive(Debug)]
enum QueryFailure {
    Unavailable,
    Unsupported,
    Invalid,
}

/// Results staged in the roster are rolled back if persistence, policy checks,
/// or cancellation prevent publication to the bridge recovery set.
struct StagedIntroductions<'a> {
    node: &'a Node,
    peers: HashMap<String, Instant>,
    committed: bool,
    rollback: Vec<RelayedRollback>,
    generation: u64,
}

impl Drop for StagedIntroductions<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let mut roster = self.node.roster.lock().expect("roster lock not poisoned");
            let restore_relays =
                self.node.bridge_generation.load(Ordering::Acquire) == self.generation;
            for undo in self.rollback.drain(..).rev() {
                roster.rollback_relayed(undo, restore_relays);
            }
        }
    }
}

#[derive(PartialEq, Eq)]
struct BridgePolicyStamp {
    directory: u64,
    privacy: u64,
    preference: Option<String>,
}

impl BridgePolicyStamp {
    fn capture(node: &Node) -> Self {
        Self {
            directory: node.bridge_generation.load(Ordering::Acquire),
            privacy: node.privacy.generation.load(Ordering::Acquire),
            preference: node
                .bridge_hub_id
                .lock()
                .expect("bridge hub lock not poisoned")
                .clone(),
        }
    }

    fn current(&self, node: &Node) -> bool {
        node.bridge_directory_allowed()
            && node.bridge_generation.load(Ordering::Acquire) == self.directory
            && node.privacy.generation.load(Ordering::Acquire) == self.privacy
            && *node
                .bridge_hub_id
                .lock()
                .expect("bridge hub lock not poisoned")
                == self.preference
    }
}

impl Node {
    pub(in crate::node) fn bridge_directory_allowed(&self) -> bool {
        !self.physical_lan_only()
            && self.post_office_enabled.load(Ordering::Acquire)
            && self.bridge_discovery_enabled.load(Ordering::Acquire)
            && !self.privacy_snapshot().invisible
    }

    /// Explicitly share this device's public presence with a discovered relay
    /// directory. Disabling immediately revokes introduced-peer recovery.
    pub fn set_bridge_discovery_enabled(&self, enabled: bool) {
        self.bridge_discovery_enabled
            .store(enabled, Ordering::Release);
        if !enabled {
            self.clear_bridge_introductions();
        }
        self.delivery_notify.notify_one();
        self.bridge_notify.notify_one();
    }

    pub(in crate::node) fn clear_bridge_introductions(&self) {
        self.bridge_generation.fetch_add(1, Ordering::AcqRel);
        self.bridge_dm_pull_peers
            .lock()
            .expect("bridge peers lock not poisoned")
            .clear();
        // Expired authorization may still have bounded presentation history.
        // An explicit policy withdrawal revokes that history too.
        let mut roster = self.roster.lock().expect("roster lock not poisoned");
        let introduced = roster
            .peers()
            .into_iter()
            .filter(|peer| {
                peer.routes
                    .iter()
                    .any(|route| route.source == crate::discovery::roster::RouteSource::PostOffice)
            })
            .map(|peer| peer.public.user_id())
            .collect::<Vec<_>>();
        for id in introduced {
            roster.forget_relayed(&id);
        }
    }

    fn expire_bridge_introductions(&self) {
        let mut peers = self
            .bridge_dm_pull_peers
            .lock()
            .expect("bridge peers lock not poisoned");
        let now = Instant::now();
        peers.retain(|_, seen| now.saturating_duration_since(*seen) < DIRECTORY_MEMBERSHIP_TTL);
        // Only recovery authorization expires here. Public origin deadlines and
        // bounded presentation history remain owned by the roster, not this map.
    }

    /// Start-to-start waves keep healthy registration independent of stalled peers.
    /// The task owns every child; retirement and network changes abort old work.
    pub(in crate::node) async fn run_bridge_directory(self: Arc<Self>) {
        let mut waves = tokio::task::JoinSet::new();
        let mut tick = tokio::time::interval(DIRECTORY_REFRESH_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut network = self.network_changes.subscribe();
        let mut policy = BridgePolicyStamp::capture(&self);
        loop {
            tokio::select! {
                _ = tick.tick() => {},
                _ = self.bridge_notify.notified() => {},
                _ = network.changed() => {
                    waves.shutdown().await;
                    self.bridge_refresh.lock().await.generation = None;
                }
                completed = waves.join_next(), if !waves.is_empty() => {
                    if let Some(Err(error)) = completed { log::debug!("directory refresh task ended: {error}"); }
                    continue;
                }
            }
            let current_policy = BridgePolicyStamp::capture(&self);
            if current_policy != policy {
                waves.shutdown().await;
                self.bridge_refresh.lock().await.generation = None;
                policy = current_policy;
            }
            if !self.bridge_directory_allowed() {
                waves.shutdown().await;
                continue;
            }
            if waves.len() < MAX_CONCURRENT_DIRECTORY_QUERIES {
                let node = self.clone();
                waves.spawn(async move { node.refresh_bridge_directory().await });
            }
        }
    }

    /// Register with each reachable, verified post office. Local and gateway
    /// relays are peers: clients use their local relay first, while registrations
    /// on both sides make cross-site introductions and mailbox drains possible.
    pub async fn refresh_bridge_directory(&self) -> usize {
        if !self.bridge_directory_allowed() {
            return 0;
        }
        let Ok(mut refresh) = self.bridge_refresh.try_lock() else {
            return 0;
        };
        let policy = BridgePolicyStamp::capture(self);
        let generation = (policy.directory, policy.privacy);
        if refresh.generation != Some(generation) {
            refresh.relays.clear();
            refresh.generation = Some(generation);
        }
        let candidates = {
            let roster = self.roster.lock().expect("roster lock not poisoned");
            self.post_office_candidates(&roster)
        };
        let now = Instant::now();
        let live: HashSet<_> = candidates
            .iter()
            .map(|relay| (relay.public.ed25519_pub, relay.public.x25519_pub))
            .collect();
        refresh.relays.retain(|key, _| live.contains(key));
        let mut candidates = candidates
            .into_iter()
            .filter_map(|relay| {
                let key = (relay.public.ed25519_pub, relay.public.x25519_pub);
                let previous = refresh
                    .relays
                    .get(&key)
                    .filter(|state| state.endpoint == relay.addr);
                if previous.is_some_and(|state| state.retry_at > now) {
                    return None;
                }
                let version = previous
                    .filter(|state| state.upgrade_at > now)
                    .map_or(0, |state| state.version);
                Some((relay, version))
            })
            .collect::<Vec<_>>();
        // Known healthy relays get the first available slot on a refresh wave.
        candidates.sort_by_key(|(relay, _)| {
            refresh
                .relays
                .get(&(relay.public.ed25519_pub, relay.public.x25519_pub))
                .is_none_or(|state| state.failures != 0)
        });
        let active = refresh.active.clone();
        drop(refresh); // Never hold scheduling metadata across network or persistence I/O.
        let mut introduced = HashSet::new();
        let mut responses = stream::iter(candidates.into_iter().map(|(relay, version)| {
            let active = active.clone();
            async move {
                let query = ActiveDirectoryQuery::acquire(
                    active,
                    (
                        relay.public.ed25519_pub,
                        relay.public.x25519_pub,
                        relay.addr,
                    ),
                )?;
                let response = self.query_bridge_relay_versioned(&relay, version).await;
                Some((relay, response, Instant::now(), query))
            }
        }))
        .buffer_unordered(MAX_CONCURRENT_DIRECTORY_QUERIES);
        while let Some(response) = responses.next().await {
            let Some((relay, response, received, _query)) = response else {
                continue;
            };
            if !policy.current(self) {
                return 0;
            }
            self.expire_bridge_introductions();
            let key = (relay.public.ed25519_pub, relay.public.x25519_pub);
            let mut refresh = self.bridge_refresh.lock().await;
            if refresh.generation != Some(generation) {
                return 0;
            }
            let response = match response {
                Ok((response, version)) => {
                    refresh.relays.insert(
                        key,
                        RelayQueryState {
                            endpoint: relay.addr,
                            failures: 0,
                            retry_at: received,
                            version,
                            upgrade_at: received + Duration::from_secs(60),
                        },
                    );
                    Some(response)
                }
                Err(_) => {
                    let failures = refresh
                        .relays
                        .get(&key)
                        .filter(|old| old.endpoint == relay.addr)
                        .map_or(1, |old| old.failures.saturating_add(1));
                    let ceiling =
                        (1u64 << failures.min(6)).min(DIRECTORY_REFRESH_INTERVAL.as_secs()) * 1000;
                    let delay = ceiling * 4 / 5
                        + (uuid::Uuid::new_v4().as_u128() as u64 % (ceiling / 5 + 1));
                    refresh.relays.insert(
                        key,
                        RelayQueryState {
                            endpoint: relay.addr,
                            failures,
                            retry_at: received + Duration::from_millis(delay),
                            version: 0,
                            upgrade_at: received,
                        },
                    );
                    None
                }
            };
            drop(refresh);
            if let Some((proofs, gateway, expiries)) = response
                && let Some(ids) = self
                    .publish_bridge_response_with_expiry(
                        &relay, proofs, gateway, received, &policy, expiries,
                    )
                    .await
            {
                introduced.extend(ids);
            }
        }
        self.expire_bridge_introductions();
        introduced.len()
    }

    #[cfg(test)]
    async fn publish_bridge_response(
        &self,
        relay: &PeerRecord,
        proofs: Vec<Announce>,
        gateway: Option<(String, u16)>,
        received: Instant,
        policy: &BridgePolicyStamp,
    ) -> Option<Vec<String>> {
        self.publish_bridge_response_with_expiry(
            relay,
            proofs,
            gateway,
            received,
            policy,
            HashMap::new(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn publish_bridge_response_with_expiry(
        &self,
        relay: &PeerRecord,
        proofs: Vec<Announce>,
        gateway: Option<(String, u16)>,
        received: Instant,
        policy: &BridgePolicyStamp,
        expiries: HashMap<String, bridge_directory::PresenceDeadline>,
    ) -> Option<Vec<String>> {
        if !policy.current(self) || !self.relay_allowed(&relay.public) {
            return None;
        }
        let mut staged = StagedIntroductions {
            node: self,
            peers: HashMap::new(),
            committed: false,
            rollback: Vec::new(),
            generation: policy.directory,
        };
        self.record_gateway(relay, &proofs, gateway);
        {
            let mut roster = self.roster.lock().expect("roster lock not poisoned");
            for proof in proofs {
                if proof.post_office || proof.public() == self.identity.public() {
                    continue;
                }
                if let Some(undo) = roster.stage_relayed(&proof, relay.addr, &self.user_id()) {
                    staged.rollback.push(undo);
                    staged.peers.insert(proof.user_id, received);
                }
            }
        }
        self.cached_peer_snapshot_async().await.ok()?;
        let mut bridge_peers = self
            .bridge_dm_pull_peers
            .lock()
            .expect("bridge peers lock not poisoned");
        if !policy.current(self)
            || !self.relay_allowed(&relay.public)
            || received.elapsed() >= DIRECTORY_MEMBERSHIP_TTL
        {
            return None;
        }
        // Publish presence only after persistence and the final policy check.
        // Provisional directory routes must not turn a concurrent UI snapshot green.
        {
            let mut roster = self.roster.lock().expect("roster lock not poisoned");
            for id in staged.peers.keys() {
                if let Some(expiry) = expiries.get(id) {
                    roster.set_directory_expiry(
                        id,
                        relay.addr,
                        expiry.expires_at,
                        expiry.origin_secs,
                    );
                }
            }
        }
        let ids = staged.peers.keys().cloned().collect();
        bridge_peers.extend(staged.peers.iter().map(|(id, seen)| (id.clone(), *seen)));
        staged.committed = true;
        drop(bridge_peers);
        self.delivery_notify.notify_one();
        Some(ids)
    }

    #[cfg(test)]
    async fn query_bridge_relay(&self, relay: &PeerRecord) -> Option<BridgeResponse> {
        self.query_bridge_relay_versioned(relay, 0)
            .await
            .ok()
            .map(|(response, _)| response)
    }

    async fn query_bridge_relay_versioned(
        &self,
        relay: &PeerRecord,
        start_version: usize,
    ) -> Result<(BridgeResponse, usize), QueryFailure> {
        // One budget includes all route dialing and any authenticated legacy
        // rejection. A timeout is never a reason to repeat four fresh dials.
        self.adaptive_control_timeout(&relay.public, async {
            if !self.relay_allowed(&relay.public) {
                return Err(QueryFailure::Unavailable);
            }
            let requests = [
                bridge_directory::freshness_request(),
                bridge_directory::request(),
                bridge_directory::gateway_request(),
                bridge_directory::legacy_request(),
            ];
            for (version, request) in requests.into_iter().enumerate().skip(start_version) {
                match self.query_bridge_relay_once(relay, request).await {
                    Ok(response) => {
                        if !self.bridge_directory_allowed() || !self.relay_allowed(&relay.public) {
                            return Err(QueryFailure::Unavailable);
                        }
                        return Ok((response, version));
                    }
                    Err(QueryFailure::Unsupported) => continue,
                    Err(error) => return Err(error),
                }
            }
            Err(QueryFailure::Unsupported)
        })
        .await
        .ok_or(QueryFailure::Unavailable)?
    }

    async fn query_bridge_relay_once(
        &self,
        relay: &PeerRecord,
        request: &[u8],
    ) -> Result<BridgeResponse, QueryFailure> {
        let started = Instant::now();
        let mut channel = self
            .privacy_dial(relay.addr, &relay.public)
            .await
            .map_err(|_| QueryFailure::Unavailable)?;
        channel
            .send(request)
            .await
            .map_err(|_| QueryFailure::Unavailable)?;
        let mut response = bridge_directory::DirectoryResponse::new(relay.public.clone());
        for page in 0..bridge_directory::MAX_MEMBERS {
            let bytes = channel.recv().await.map_err(|error| {
                if page == 0 && matches!(&error, crate::transport::TransportError::Io(io)
                    if matches!(io.kind(), std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset)) {
                    QueryFailure::Unsupported
                } else { QueryFailure::Unavailable }
            })?;
            match response.add_page(&bytes).ok_or(QueryFailure::Invalid)? {
                bridge_directory::DirectoryProgress::More => {}
                bridge_directory::DirectoryProgress::Complete(snapshot) => {
                    self.observe_control_exchange(&channel, started);
                    return Ok((snapshot.0, snapshot.1, response.expiries));
                }
            }
        }
        Err(QueryFailure::Invalid)
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
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn expired_directory_membership_retains_origin_bound_presentation_until_explicit_disable()
    {
        let temp = tempfile::tempdir().unwrap();
        let node = directory_client(temp.path());
        let member = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&member, &Account::generate(), "remote", 1);
        let observed = Instant::now() - Duration::from_secs(16);
        let relay = "127.0.0.1:47479".parse().unwrap();
        {
            let mut roster = node.roster.lock().unwrap();
            roster.update_relayed(&proof, relay, &node.user_id());
            roster.set_directory_expiry(
                &member.user_id(),
                relay,
                observed + DIRECTORY_MEMBERSHIP_TTL,
                1,
            );
        }
        node.bridge_dm_pull_peers
            .lock()
            .unwrap()
            .insert(member.user_id(), observed);
        node.expire_bridge_introductions();
        assert!(
            !node
                .bridge_dm_pull_peers
                .lock()
                .unwrap()
                .contains_key(&member.user_id()),
            "expired membership must not authorize recovery"
        );
        {
            let roster = node.roster.lock().unwrap();
            let peer = roster
                .get(&member.user_id())
                .expect("expiry is not authoritative withdrawal");
            assert!(peer.presence_evidence(Instant::now()).is_empty());
            assert_eq!(
                peer.reconnect_remaining(observed + Duration::from_secs(119)),
                Duration::from_secs(1)
            );
            assert_eq!(
                peer.reconnect_remaining(observed + Duration::from_secs(120)),
                Duration::ZERO
            );
        }
        node.set_bridge_discovery_enabled(false);
        assert!(
            node.roster.lock().unwrap().get(&member.user_id()).is_none(),
            "explicit disable clears even expired introduction history"
        );
    }

    #[tokio::test]
    async fn healthy_directory_renews_while_a_previous_relay_query_is_still_pending() {
        assert_directory_refresh_independence(false).await;
    }

    #[tokio::test]
    async fn participant_directory_waves_renew_healthy_registration_before_a_slow_query_finishes() {
        assert_directory_refresh_independence(true).await;
    }

    async fn assert_directory_refresh_independence(runtime: bool) {
        use std::sync::atomic::AtomicUsize;
        let temp = tempfile::tempdir().unwrap();
        let node = directory_client(temp.path());
        let healthy = DeviceIdentity::generate();
        let slow = DeviceIdentity::generate();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let stalled_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        for (identity, address) in [
            (&healthy, listener.local_addr().unwrap()),
            (&slow, stalled_listener.local_addr().unwrap()),
        ] {
            let proof = Announce::new_post_office_with_account(
                identity,
                &Account::generate(),
                "relay",
                address.port(),
            );
            node.roster
                .lock()
                .unwrap()
                .update(&proof, address.ip(), &node.user_id());
        }
        let healthy_count = Arc::new(AtomicUsize::new(0));
        let slow_count = Arc::new(AtomicUsize::new(0));
        let count = healthy_count.clone();
        let directory = crate::postoffice::federation::FederationDirectory::new();
        let member = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&member, &Account::generate(), "member", 1);
        directory
            .shared()
            .lock()
            .unwrap()
            .register(&member.public(), Some(&proof), true, Instant::now())
            .unwrap();
        let keys = healthy.secret_bytes();
        let office = Arc::new(Mutex::new(
            crate::postoffice::PostOffice::open(&temp.path().join("office"), "pw", healthy)
                .unwrap(),
        ));
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let channel = crate::transport::SecureChannel::accept(
                    stream,
                    &DeviceIdentity::from_secret_bytes(keys.0, keys.1),
                )
                .await
                .unwrap();
                super::super::postbox::serve_relay_or_bridge_connection(
                    channel,
                    office.clone(),
                    Some(directory.shared()),
                )
                .await;
                count.fetch_add(1, Ordering::SeqCst);
            }
        });
        let count = slow_count.clone();
        let stalled = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = stalled_listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        let keys = slow.secret_bytes();
                        let count = count.clone();
                        tasks.spawn(async move {
                            let mut channel = crate::transport::SecureChannel::accept(stream, &DeviceIdentity::from_secret_bytes(keys.0, keys.1)).await.unwrap();
                            assert!(bridge_directory::is_request(&channel.recv().await.unwrap()));
                            count.fetch_add(1, Ordering::SeqCst);
                            let _ = channel.recv().await;
                        });
                    }
                    _ = tasks.join_next(), if !tasks.is_empty() => {},
                }
            }
        });
        let client = node.clone();
        let first = tokio::spawn(async move {
            if runtime {
                client.run_bridge_directory().await;
                0
            } else {
                client.refresh_bridge_directory().await
            }
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while healthy_count.load(Ordering::SeqCst) < 1
                || slow_count.load(Ordering::SeqCst) < 1
                || !node
                    .bridge_dm_pull_peers
                    .lock()
                    .unwrap()
                    .contains_key(&member.user_id())
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let next = if runtime {
            tokio::time::timeout(Duration::from_secs(7), async {
                while healthy_count.load(Ordering::SeqCst) < 2 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
        } else {
            tokio::time::timeout(Duration::from_secs(3), node.refresh_bridge_directory())
                .await
                .map(|_| ())
        };
        let renewed = healthy_count.load(Ordering::SeqCst) >= 2;
        let pending = !first.is_finished();
        let duplicated = slow_count.load(Ordering::SeqCst);
        let policy_refresh = if runtime {
            node.set_bridge_discovery_enabled(false);
            node.set_bridge_discovery_enabled(true);
            tokio::time::timeout(Duration::from_secs(3), async {
                while slow_count.load(Ordering::SeqCst) < 2 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
        } else {
            Ok(())
        };
        first.abort();
        let _ = first.await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while !node
                .bridge_refresh
                .lock()
                .await
                .active
                .lock()
                .unwrap()
                .is_empty()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("retirement releases every query activity slot");
        server.abort();
        let _ = server.await;
        stalled.abort();
        let _ = stalled.await;
        assert!(
            next.is_ok() && renewed,
            "a second healthy registration cannot wait for the previous stalled relay"
        );
        assert!(
            pending,
            "the original progressing/slow query was not cancelled to fake a refresh"
        );
        assert_eq!(duplicated, 1, "one active query per relay");
        assert!(
            policy_refresh.is_ok(),
            "an off/on policy transition must retire old generation queries immediately"
        );
    }

    #[tokio::test]
    async fn malformed_directory_response_does_not_retry_other_versions() {
        let dir = tempfile::tempdir().unwrap();
        let node = directory_client(dir.path());
        let relay = crate::identity::device::DeviceIdentity::generate();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let proof = Announce::new_post_office_with_account(
            &relay,
            &Account::generate(),
            "relay",
            addr.port(),
        );
        node.roster
            .lock()
            .unwrap()
            .update(&proof, addr.ip(), &node.user_id());
        let peer = node
            .roster
            .lock()
            .unwrap()
            .get(&relay.user_id())
            .unwrap()
            .clone();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = count.clone();
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                seen.fetch_add(1, Ordering::SeqCst);
                let mut channel = crate::transport::SecureChannel::accept(stream, &relay)
                    .await
                    .unwrap();
                let _ = channel.recv().await.unwrap();
                channel
                    .send(b"malformed authenticated directory response")
                    .await
                    .unwrap();
            }
        });
        assert!(node.query_bridge_relay(&peer).await.is_none());
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "malformed replies are failures, not version negotiation"
        );
        node.refresh_bridge_directory().await;
        assert_eq!(count.load(Ordering::SeqCst), 2);
        node.refresh_bridge_directory().await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "a failed relay must back off independently"
        );
        server.abort();
    }

    #[tokio::test]
    async fn authenticated_legacy_rejection_is_negotiated_once_and_cached() {
        let dir = tempfile::tempdir().unwrap();
        let node = directory_client(dir.path());
        let relay = DeviceIdentity::generate();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let proof = Announce::new_post_office_with_account(
            &relay,
            &Account::generate(),
            "relay",
            addr.port(),
        );
        node.roster
            .lock()
            .unwrap()
            .update(&proof, addr.ip(), &node.user_id());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = requests.clone();
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut channel = crate::transport::SecureChannel::accept(stream, &relay)
                    .await
                    .unwrap();
                let request = channel.recv().await.unwrap();
                received.lock().unwrap().push(request.clone());
                if request == bridge_directory::freshness_request() {
                    continue;
                }
                for page in bridge_directory::encode_for_request(&request, &[], None).unwrap() {
                    channel.send(&page).await.unwrap();
                }
            }
        });
        node.refresh_bridge_directory().await;
        node.refresh_bridge_directory().await;
        assert_eq!(
            *requests.lock().unwrap(),
            vec![
                bridge_directory::freshness_request().to_vec(),
                bridge_directory::request().to_vec(),
                bridge_directory::request().to_vec(),
            ]
        );
        server.abort();
    }

    fn directory_client(path: &std::path::Path) -> Arc<Node> {
        let own = DeviceIdentity::generate();
        let account = Account::generate();
        let own_proof = Announce::new_with_account(&own, &account, "client", 1);
        let dummy = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "dummy",
            1,
        );
        let (node, _, _) = super::super::test_support::node_with_files(path, own, account, &dummy);
        node.configure_privacy(
            path,
            "pw",
            &own_proof,
            Arc::new(crate::discovery::DiscoveryVisibility::default()),
        )
        .unwrap();
        node.set_bridge_discovery_enabled(true);
        node
    }

    #[tokio::test]
    async fn tcp_failure_does_not_erase_fresh_routed_presence() {
        let temp = tempfile::tempdir().unwrap();
        let node = directory_client(temp.path());
        let device = DeviceIdentity::generate();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let proof = Announce::new_with_account(&device, &Account::generate(), "Peer", addr.port());
        node.roster
            .lock()
            .unwrap()
            .update_routed(&proof, addr.ip(), &node.user_id());
        assert!(node.privacy_dial(addr, &device.public()).await.is_err());
        let peers = node.roster.lock().unwrap().peers();
        let peer = peers.iter().find(|p| p.public == device.public()).unwrap();
        assert!(
            peer.presence_evidence(Instant::now())
                .iter()
                .any(|e| e.0 == RouteSource::RoutedQuery)
        );
        assert!(!peer.has_lan_route());
    }

    #[tokio::test]
    async fn physical_scope_revokes_bridge_imports_without_changing_bridge_preference() {
        let temp = tempfile::tempdir().unwrap();
        let node = directory_client(temp.path());
        assert!(node.bridge_directory_allowed());
        let member = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&member, &Account::generate(), "remote", 1);
        node.roster.lock().unwrap().update_relayed(
            &proof,
            "127.0.0.1:47475".parse().unwrap(),
            &node.user_id(),
        );
        node.bridge_dm_pull_peers
            .lock()
            .unwrap()
            .insert(member.user_id(), Instant::now());
        let stamp = BridgePolicyStamp::capture(&node);
        node.set_physical_lan_only(true).await;
        assert!(node.physical_lan_only());
        assert!(!node.bridge_directory_allowed());
        assert!(!stamp.current(&node));
        assert!(node.bridge_dm_pull_peers.lock().unwrap().is_empty());
        assert!(node.roster.lock().unwrap().get(&member.user_id()).is_none());
        assert!(node.bridge_discovery_enabled.load(Ordering::Acquire));
        node.set_physical_lan_only(false).await;
        assert!(node.bridge_directory_allowed());
        assert!(
            !stamp.current(&node),
            "scope off/on must not resurrect in-flight introductions"
        );
    }

    #[tokio::test]
    async fn staged_introductions_roll_back_on_cancellation_or_policy_aba() {
        for cancel in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let node = directory_client(temp.path());
            let relay = DeviceIdentity::generate();
            let relay_proof =
                Announce::new_post_office_with_account(&relay, &Account::generate(), "relay", 1);
            node.roster.lock().unwrap().update(
                &relay_proof,
                "127.0.0.1".parse().unwrap(),
                &node.user_id(),
            );
            let route = node
                .roster
                .lock()
                .unwrap()
                .get(&relay.user_id())
                .unwrap()
                .clone();
            let member = DeviceIdentity::generate();
            let proof = Announce::new_with_account(&member, &Account::generate(), "remote", 1);
            let (started, waiting) = tokio::sync::oneshot::channel();
            let (release, resume) = std::sync::mpsc::channel();
            let resume = Mutex::new(resume);
            *node.peer_snapshot_hook.lock().unwrap() = Some(Box::new(move || {
                let _ = started.send(());
                let _ = resume.lock().unwrap().recv();
            }));
            let client = node.clone();
            let policy = BridgePolicyStamp::capture(&node);
            let expiries = HashMap::from([(
                proof.user_id.clone(),
                bridge_directory::PresenceDeadline {
                    expires_at: Instant::now() + DIRECTORY_MEMBERSHIP_TTL,
                    origin_secs: 15,
                },
            )]);
            let publish = tokio::spawn(async move {
                client
                    .publish_bridge_response_with_expiry(
                        &route,
                        vec![proof],
                        None,
                        Instant::now(),
                        &policy,
                        expiries,
                    )
                    .await
            });
            tokio::time::timeout(Duration::from_secs(3), waiting)
                .await
                .unwrap()
                .unwrap();
            {
                let roster = node.roster.lock().unwrap();
                let staged = roster.get(&member.user_id()).unwrap();
                assert!(
                    staged.presence_evidence(Instant::now()).is_empty(),
                    "uncommitted directory routes cannot publish presence"
                );
            }
            if cancel {
                publish.abort();
                assert!(publish.await.unwrap_err().is_cancelled());
                release.send(()).unwrap();
            } else {
                node.set_bridge_discovery_enabled(false);
                node.set_bridge_discovery_enabled(true);
                release.send(()).unwrap();
                assert!(
                    publish.await.unwrap().is_none(),
                    "off/on must invalidate an earlier query"
                );
            }
            assert!(node.roster.lock().unwrap().get(&member.user_id()).is_none());
            assert!(
                !node
                    .bridge_dm_pull_peers
                    .lock()
                    .unwrap()
                    .contains_key(&member.user_id())
            );
        }
    }

    #[tokio::test]
    async fn cancelled_reply_preserves_committed_routes_and_original_refresh_times() {
        for same_relay in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let node = directory_client(temp.path());
            let member = DeviceIdentity::generate();
            let proof = Announce::new_with_account(&member, &Account::generate(), "remote", 1);
            let mut relays = Vec::new();
            for port in [47001, 47002] {
                let identity = DeviceIdentity::generate();
                let announce = Announce::new_post_office_with_account(
                    &identity,
                    &Account::generate(),
                    "relay",
                    port,
                );
                let mut roster = node.roster.lock().unwrap();
                roster.update(&announce, "127.0.0.1".parse().unwrap(), &node.user_id());
                relays.push(roster.get(&identity.user_id()).unwrap().clone());
            }
            let policy = BridgePolicyStamp::capture(&node);
            node.publish_bridge_response(
                &relays[0],
                vec![proof.clone()],
                None,
                Instant::now(),
                &policy,
            )
            .await
            .unwrap();
            // Fill the route cap, including an independent direct route. Staging
            // another relay may evict an old route, which cancellation must restore.
            let before = {
                let mut roster = node.roster.lock().unwrap();
                for port in 47100..47106 {
                    roster.update_relayed(
                        &proof,
                        format!("127.0.0.1:{port}").parse().unwrap(),
                        &node.user_id(),
                    );
                }
                roster.update(&proof, "127.0.0.2".parse().unwrap(), &node.user_id());
                roster.get(&member.user_id()).unwrap().routes.clone()
            };
            let (started, waiting) = tokio::sync::oneshot::channel();
            let (release, resume) = std::sync::mpsc::channel();
            let resume = Mutex::new(resume);
            *node.peer_snapshot_hook.lock().unwrap() = Some(Box::new(move || {
                let _ = started.send(());
                let _ = resume.lock().unwrap().recv();
            }));
            let client = node.clone();
            let relay = relays[usize::from(!same_relay)].clone();
            let publish = tokio::spawn(async move {
                client
                    .publish_bridge_response(&relay, vec![proof], None, Instant::now(), &policy)
                    .await
            });
            tokio::time::timeout(Duration::from_secs(3), waiting)
                .await
                .unwrap()
                .unwrap();
            // A concurrent direct refresh must also survive the rollback.
            node.roster
                .lock()
                .unwrap()
                .mark_authenticated(&member.public(), "127.0.0.2:1".parse().unwrap());
            publish.abort();
            assert!(publish.await.unwrap_err().is_cancelled());
            release.send(()).unwrap();
            let roster = node.roster.lock().unwrap();
            let after = roster
                .get(&member.user_id())
                .expect("earlier committed peer survives");
            for old in before
                .iter()
                .filter(|route| route.source == RouteSource::PostOffice)
            {
                assert!(
                    after.routes.iter().any(|route| route.addr == old.addr
                        && route.source == old.source
                        && route.last_seen == old.last_seen),
                    "prior relay route and timestamp survive cancellation: {}",
                    old.addr
                );
            }
            assert!(after.has_lan_route());
            assert!(
                after
                    .routes
                    .iter()
                    .any(|route| route.source == RouteSource::Lan
                        && route.last_authenticated.is_some()),
                "concurrent direct authentication survives rollback"
            );
            assert!(after.routes.len() <= 8);
            assert!(
                node.bridge_dm_pull_peers
                    .lock()
                    .unwrap()
                    .contains_key(&member.user_id())
            );
        }
    }

    #[tokio::test]
    async fn directory_refresh_reaches_relays_beyond_the_first_eight() {
        let temp = tempfile::tempdir().unwrap();
        let node = directory_client(temp.path());
        let mut servers = Vec::new();
        for index in 0..9 {
            let id = DeviceIdentity::generate();
            let office_account = Account::generate();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let announce =
                Announce::new_post_office_with_account(&id, &office_account, "relay", addr.port());
            node.roster
                .lock()
                .unwrap()
                .update(&announce, addr.ip(), &node.user_id());
            let (ed, x) = id.secret_bytes();
            let office = Arc::new(Mutex::new(
                crate::postoffice::PostOffice::open(
                    &temp.path().join(format!("relay-{index}")),
                    "pw",
                    id,
                )
                .unwrap(),
            ));
            let directory = crate::postoffice::federation::FederationDirectory::new();
            let member = DeviceIdentity::generate();
            let member_proof =
                Announce::new_with_account(&member, &Account::generate(), "remote", 1);
            directory
                .shared()
                .lock()
                .unwrap()
                .register(&member.public(), Some(&member_proof), true, Instant::now())
                .unwrap();
            servers.push(tokio::spawn(
                super::super::postbox::run_federated_relay_accept_loop_with_directory(
                    DeviceIdentity::from_secret_bytes(ed, x),
                    listener,
                    office,
                    Arc::new(std::collections::HashSet::new()),
                    directory,
                ),
            ));
        }
        // This counts completed authenticated directory queries, not ordinary
        // LAN discoveries, so the ninth relay cannot be silently skipped.
        let count = node.refresh_bridge_directory().await;
        assert_eq!(
            count, 9,
            "all eligible relays must be queried with bounded concurrency"
        );
        let peers = node.roster.lock().unwrap().peers();
        let introduced: Vec<_> = peers
            .iter()
            .filter(|p| {
                !p.post_office && p.routes.iter().any(|r| r.source == RouteSource::PostOffice)
            })
            .collect();
        assert_eq!(introduced.len(), 9);
        for peer in introduced {
            assert!(
                !peer.has_direct_route(),
                "directory introduction is not a recipient route"
            );
            assert!(
                peer.presence_evidence(Instant::now())
                    .iter()
                    .any(|e| e.0 == RouteSource::PostOffice),
                "authenticated loopback directory query retains fresh source evidence"
            );
        }
        for server in servers {
            server.abort();
        }
    }

    #[tokio::test]
    async fn healthy_directory_is_published_while_another_query_stalls() {
        let temp = tempfile::tempdir().unwrap();
        let node = directory_client(temp.path());
        let healthy = DeviceIdentity::generate();
        let slow = DeviceIdentity::generate();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let stalled_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        for (identity, address) in [
            (&healthy, listener.local_addr().unwrap()),
            (&slow, stalled_listener.local_addr().unwrap()),
        ] {
            let proof = Announce::new_post_office_with_account(
                identity,
                &Account::generate(),
                "relay",
                address.port(),
            );
            node.roster
                .lock()
                .unwrap()
                .update(&proof, address.ip(), &node.user_id());
        }
        node.set_bridge_hub_id(Some(&slow.user_id())).unwrap();
        assert_eq!(
            node.post_office_candidates(&node.roster.lock().unwrap())[0].public,
            slow.public(),
            "the stalled query must precede the healthy query"
        );
        let member = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&member, &Account::generate(), "remote", 1);
        let directory = crate::postoffice::federation::FederationDirectory::new();
        directory
            .shared()
            .lock()
            .unwrap()
            .register(&member.public(), Some(&proof), true, Instant::now())
            .unwrap();
        let (ed, x) = healthy.secret_bytes();
        let office = Arc::new(Mutex::new(
            crate::postoffice::PostOffice::open(&temp.path().join("relay"), "pw", healthy).unwrap(),
        ));
        let (served, response_served) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let channel = crate::transport::SecureChannel::accept(
                stream,
                &DeviceIdentity::from_secret_bytes(ed, x),
            )
            .await
            .unwrap();
            super::super::postbox::serve_relay_or_bridge_connection(
                channel,
                office,
                Some(directory.shared()),
            )
            .await;
            // Directory serving returns after writing every response page; it
            // does not wait for the client to close its authenticated channel.
            let _ = served.send(());
        });
        let release = Arc::new(tokio::sync::Notify::new());
        let resume = release.clone();
        let (accepted, query_accepted) = tokio::sync::oneshot::channel();
        let stalled = tokio::spawn(async move {
            let (stream, _) = stalled_listener.accept().await.unwrap();
            let mut channel = crate::transport::SecureChannel::accept(stream, &slow)
                .await
                .unwrap();
            let request = channel.recv().await.unwrap();
            assert!(bridge_directory::is_request(&request));
            let _ = accepted.send(());
            tokio::select! {
                _ = resume.notified() => {},
                result = channel.recv() => panic!("stalled query ended before release: {result:?}"),
            }
        });
        let client = node.clone();
        let refresh = tokio::spawn(async move { client.refresh_bridge_directory().await });
        // Separate authenticated request/response setup from publication. The
        // publication bound starts only when the healthy response was served.
        let setup = tokio::time::timeout(BRIDGE_TIMEOUT, async {
            let healthy = response_served.await;
            let stalled = query_accepted.await;
            (healthy, stalled)
        })
        .await;
        let published = tokio::time::timeout(Duration::from_secs(3), async {
            while !node
                .bridge_dm_pull_peers
                .lock()
                .unwrap()
                .contains_key(&member.user_id())
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        let refresh_pending = !refresh.is_finished();
        let stall_held = !stalled.is_finished();
        release.notify_one();
        refresh.abort();
        let _ = refresh.await;
        server.abort();
        let _ = server.await;
        stalled.abort();
        let _ = stalled.await;
        let (healthy_response, stalled_request) =
            setup.expect("authenticated healthy response and stalled query complete setup");
        healthy_response.expect("healthy response was fully served");
        stalled_request.expect("slow server accepted the directory request");
        assert!(
            stall_held,
            "stalled server must remain held until publication"
        );
        assert!(
            refresh_pending,
            "healthy publication must precede refresh completion"
        );
        assert!(
            published.is_ok(),
            "a stalled relay must not delay a healthy introduction's publication"
        );
    }

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
