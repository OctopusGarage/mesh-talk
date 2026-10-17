//! Discover opt-in peers through an already discovered post office. All claims
//! remain self-signed, and the relay endpoint is kept separate from direct routes.

use super::*;
use crate::discovery::Announce;
use crate::discovery::roster::{PeerRecord, RelayedRollback};
use crate::node::bridge_directory;
use futures_util::{StreamExt, stream};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

type BridgeResponse = (
    Vec<Announce>,
    Option<(String, u16)>,
    HashMap<String, Instant>,
);

const BRIDGE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CONCURRENT_DIRECTORY_QUERIES: usize = 8;
const DIRECTORY_MEMBERSHIP_TTL: Duration = Duration::from_secs(45);

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
        if enabled {
            self.bridge_notify.notify_one();
        }
    }

    pub(in crate::node) fn clear_bridge_introductions(&self) {
        self.bridge_generation.fetch_add(1, Ordering::AcqRel);
        let introduced = std::mem::take(
            &mut *self
                .bridge_dm_pull_peers
                .lock()
                .expect("bridge peers lock not poisoned"),
        );
        self.forget_bridge_peers(&introduced);
    }

    fn forget_bridge_peers(&self, introduced: &HashMap<String, Instant>) {
        let mut roster = self.roster.lock().expect("roster lock not poisoned");
        for id in introduced.keys() {
            roster.forget_relayed(id);
        }
    }

    fn expire_bridge_introductions(&self) {
        let mut peers = self
            .bridge_dm_pull_peers
            .lock()
            .expect("bridge peers lock not poisoned");
        let now = Instant::now();
        let expired: Vec<_> = peers
            .iter()
            .filter(|(_, seen)| now.saturating_duration_since(**seen) >= DIRECTORY_MEMBERSHIP_TTL)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            peers.remove(id);
        }
        drop(peers);
        let mut roster = self.roster.lock().expect("roster lock not poisoned");
        for id in expired {
            roster.forget_relayed(&id);
        }
    }

    /// Register with each reachable, verified post office. Local and gateway
    /// relays are peers: clients use their local relay first, while registrations
    /// on both sides make cross-site introductions and mailbox drains possible.
    pub async fn refresh_bridge_directory(&self) -> usize {
        if !self.bridge_directory_allowed() {
            return 0;
        }
        let Ok(_refresh) = self.bridge_refresh.try_lock() else {
            return 0;
        };
        let policy = BridgePolicyStamp::capture(self);
        let candidates = {
            let roster = self.roster.lock().expect("roster lock not poisoned");
            self.post_office_candidates(&roster)
        };
        let mut introduced = HashSet::new();
        let mut responses = stream::iter(candidates.into_iter().map(|relay| async move {
            let response = self.query_bridge_relay(&relay).await;
            (relay, response, Instant::now())
        }))
        .buffer_unordered(MAX_CONCURRENT_DIRECTORY_QUERIES);
        while let Some((relay, response, received)) = responses.next().await {
            if !policy.current(self) {
                return 0;
            }
            self.expire_bridge_introductions();
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
        expiries: HashMap<String, Instant>,
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
                    roster.set_directory_expiry(id, relay.addr, *expiry);
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

    async fn query_bridge_relay(&self, relay: &PeerRecord) -> Option<BridgeResponse> {
        if !self.relay_allowed(&relay.public) {
            return None;
        }
        let mut response = None;
        for request in [
            bridge_directory::freshness_request(),
            bridge_directory::request(),
            bridge_directory::gateway_request(),
            bridge_directory::legacy_request(),
        ] {
            if let Some(snapshot) = self.query_bridge_relay_once(relay, request).await {
                response = Some(snapshot);
                break;
            }
        }
        let response = response?;
        if !self.bridge_directory_allowed() || !self.relay_allowed(&relay.public) {
            return None;
        }
        Some(response)
    }

    async fn query_bridge_relay_once(
        &self,
        relay: &PeerRecord,
        request: &[u8],
    ) -> Option<BridgeResponse> {
        let mut channel = self.privacy_dial(relay.addr, &relay.public).await.ok()?;
        let response = tokio::time::timeout(BRIDGE_TIMEOUT, async {
            channel.send(request).await.ok()?;
            let mut response = bridge_directory::DirectoryResponse::new(relay.public.clone());
            for _ in 0..bridge_directory::MAX_MEMBERS {
                let bytes = channel.recv().await.ok()?;
                match response.add_page(&bytes)? {
                    bridge_directory::DirectoryProgress::More => {}
                    bridge_directory::DirectoryProgress::Complete(snapshot) => {
                        return Some((snapshot.0, snapshot.1, response.expiries));
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
    use std::sync::{Arc, Mutex};

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
                Instant::now() + Duration::from_secs(45),
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
