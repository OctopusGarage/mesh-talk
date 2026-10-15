//! The roster: the set of currently-known peers, keyed by `user_id`.

use crate::discovery::announce::Announce;
use crate::identity::device::PublicIdentity;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// A peer's stable fingerprint id.
pub type UserId = String;

/// Origin describes how an address was learned, never who the peer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteSource {
    Lan,
    RoutedQuery,
    Gateway,
    Authenticated,
    /// Address belongs to a post office, not this peer's TCP listener.
    PostOffice,
}

#[derive(Debug, Clone)]
pub struct PeerEndpoint {
    pub addr: SocketAddr,
    pub source: RouteSource,
    pub last_seen: Instant,
    pub last_authenticated: Option<Instant>,
}

const MAX_ROUTES_PER_PEER: usize = 8;

/// What we know about a discovered peer: its keys, where to reach it, whether it
/// serves as a post office, and when we last heard from it.
#[derive(Debug, Clone)]
pub struct PeerRecord {
    pub public: PublicIdentity,
    pub addr: SocketAddr,
    /// Bounded alternative paths for the same verified public identity.
    pub routes: Vec<PeerEndpoint>,
    pub name: String,
    pub post_office: bool,
    /// The account this device belongs to, from its verified cert (`None` if the
    /// device advertised no account). Devices sharing an `account_id` are the same
    /// user's devices.
    pub account_id: Option<String>,
    pub last_seen: Instant,
}

/// The outcome of feeding an announce to the roster. Distinguishing `New` from
/// `Refreshed` lets the listener unicast a reply only on first sight of a peer
/// (LocalSend's announce/response), avoiding a reply storm on every re-announce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// The announce was invalid or our own — nothing recorded.
    Rejected,
    /// A peer we had not seen before was recorded for the first time.
    New,
    /// A peer we already knew re-announced (record refreshed).
    Refreshed,
}

impl UpdateOutcome {
    /// True if the announce was accepted (recorded), whether new or a refresh.
    pub fn accepted(self) -> bool {
        !matches!(self, UpdateOutcome::Rejected)
    }
}

/// Maximum distinct peers the roster will hold. A hostile LAN can announce thousands
/// of distinct (validly-signed) fake `user_id`s; without a cap the roster — and so the
/// process memory — would grow unbounded. 1024 is far above any plausible real LAN.
/// When full, recording a NEW peer evicts the least-recently-seen existing one (LRU),
/// so a flood of fakes can churn the table but can't grow it without bound. Refreshing
/// or looking up a KNOWN peer is never blocked by the cap.
pub const MAX_PEERS: usize = 1024;

/// The known-peers map.
#[derive(Default)]
pub struct Roster {
    peers: HashMap<UserId, PeerRecord>,
    announcements: HashMap<UserId, Announce>,
    historical: BTreeMap<UserId, Announce>,
    conflicting_history: HashSet<UserId>,
    changes: std::sync::Arc<Notify>,
}

impl Roster {
    /// Wakes recovery when discovery learns a new peer or a peer's route changes.
    /// Notifications coalesce; consumers must inspect the roster after waking.
    pub fn changes(&self) -> std::sync::Arc<Notify> {
        std::sync::Arc::clone(&self.changes)
    }

    pub(crate) fn historical_announcement(&self, public: &PublicIdentity) -> Option<&Announce> {
        let id = public.user_id();
        self.historical
            .get(&id)
            .filter(|a| !self.conflicting_history.contains(&id) && a.public() == *public)
    }
    pub(crate) fn next_historical_after(&self, cursor: Option<&str>) -> Option<Announce> {
        let start = cursor.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded);
        self.historical
            .range::<str, _>((start, std::ops::Bound::Unbounded))
            .chain(self.historical.range::<str, _>((
                std::ops::Bound::Unbounded,
                std::ops::Bound::Included(cursor.unwrap_or("")),
            )))
            .find(|(id, _)| !self.conflicting_history.contains(*id))
            .map(|(_, a)| a.clone())
    }
    pub(crate) fn historical_by_author(&self, author: &[u8; 32]) -> Option<Announce> {
        let id = PublicIdentity::user_id_from(author);
        self.historical
            .get(&id)
            .filter(|a| a.ed25519_pub == *author && !self.conflicting_history.contains(&id))
            .cloned()
    }
    /// Verify and record an announce received from `source_ip`. Returns an
    /// [`UpdateOutcome`]: `Rejected` if inauthentic or our own `self_user_id`,
    /// `New` on first sight of the peer, `Refreshed` if it was already known.
    /// The peer's address is `(source_ip, announce.tcp_port)` — the announce names
    /// the TCP port; the IP comes from the datagram source.
    pub fn update(
        &mut self,
        announce: &Announce,
        source_ip: IpAddr,
        self_user_id: &str,
    ) -> UpdateOutcome {
        self.update_from(
            announce,
            source_ip,
            announce.tcp_port,
            self_user_id,
            RouteSource::Lan,
        )
    }

    pub fn update_routed(
        &mut self,
        announce: &Announce,
        source_ip: IpAddr,
        self_user_id: &str,
    ) -> UpdateOutcome {
        self.update_from(
            announce,
            source_ip,
            announce.tcp_port,
            self_user_id,
            RouteSource::RoutedQuery,
        )
    }

    /// Signed relay proof supplied over a Noise-authenticated local bridge
    /// directory. Its TCP port is served locally by that relay's gateway.
    pub fn update_gateway(
        &mut self,
        announce: &Announce,
        gateway_ip: IpAddr,
        gateway_port: u16,
        self_user_id: &str,
    ) -> UpdateOutcome {
        if !announce.post_office || announce.account_id().is_none() || gateway_port == 0 {
            return UpdateOutcome::Rejected;
        }
        self.update_from(
            announce,
            gateway_ip,
            gateway_port,
            self_user_id,
            RouteSource::Gateway,
        )
    }

    /// A signed ordinary peer introduced by a relay. The relay's address is
    /// useful for opaque mailbox delivery but is never a direct peer endpoint.
    /// Post offices need their own verified gateway route instead.
    pub fn update_relayed(
        &mut self,
        announce: &Announce,
        relay: SocketAddr,
        self_user_id: &str,
    ) -> UpdateOutcome {
        if announce.user_id == self_user_id
            || !announce.verify()
            || announce.post_office
            || announce.account_id().is_none()
            || announce.tcp_port == 0
            || announce.name.len() > 1024
            || self.peers.get(&announce.user_id).is_some_and(|old| {
                old.public != announce.public() || old.account_id != announce.account_id()
            })
            || self.historical.get(&announce.user_id).is_some_and(|old| {
                old.public() != announce.public() || old.account_id() != announce.account_id()
            })
        {
            return UpdateOutcome::Rejected;
        }
        let now = Instant::now();
        let existing = self.peers.get(&announce.user_id);
        let existed = existing.is_some();
        let mut routes = existing.map(|peer| peer.routes.clone()).unwrap_or_default();
        let route_changed = !routes
            .iter()
            .any(|route| route.addr == relay && route.source == RouteSource::PostOffice);
        routes.retain(|route| route.addr != relay || route.source != RouteSource::PostOffice);
        routes.push(PeerEndpoint {
            addr: relay,
            source: RouteSource::PostOffice,
            last_seen: now,
            last_authenticated: None,
        });
        if routes.len() > MAX_ROUTES_PER_PEER {
            routes.remove(0);
        }
        if !existed
            && self.peers.len() >= MAX_PEERS
            && let Some(oldest) = self
                .peers
                .iter()
                .min_by_key(|(_, peer)| peer.last_seen)
                .map(|(id, _)| id.clone())
        {
            self.peers.remove(&oldest);
            self.announcements.remove(&oldest);
        }
        if self.historical.len() < MAX_PEERS || self.historical.contains_key(&announce.user_id) {
            self.historical
                .insert(announce.user_id.clone(), announce.clone());
        }
        self.announcements
            .insert(announce.user_id.clone(), announce.clone());
        let direct = routes
            .iter()
            .rev()
            .find(|route| route.source != RouteSource::PostOffice)
            .map(|route| route.addr);
        self.peers.insert(
            announce.user_id.clone(),
            PeerRecord {
                public: announce.public(),
                addr: direct.unwrap_or(relay),
                routes,
                name: announce.name.clone(),
                post_office: false,
                account_id: announce.account_id(),
                last_seen: now,
            },
        );
        if route_changed {
            self.changes.notify_one();
        }
        if existed {
            UpdateOutcome::Refreshed
        } else {
            UpdateOutcome::New
        }
    }

    fn update_from(
        &mut self,
        announce: &Announce,
        source_ip: IpAddr,
        port: u16,
        self_user_id: &str,
        source: RouteSource,
    ) -> UpdateOutcome {
        if announce.user_id == self_user_id {
            return UpdateOutcome::Rejected; // self-filter
        }
        if !announce.verify() {
            return UpdateOutcome::Rejected;
        }
        if self.historical.get(&announce.user_id).is_some_and(|old| {
            old.public() != announce.public()
                || (old.account_id().is_some() && old.account_id() != announce.account_id())
        }) {
            self.conflicting_history.insert(announce.user_id.clone());
        } else if self.historical.len() < MAX_PEERS
            || self.historical.contains_key(&announce.user_id)
        {
            self.historical
                .insert(announce.user_id.clone(), announce.clone());
        }
        let addr = SocketAddr::new(source_ip, port);
        let previous = self.peers.get(&announce.user_id);
        let existed = previous.is_some();
        let route_changed = previous.is_none_or(|peer| {
            peer.public != announce.public() || !peer.routes.iter().any(|route| route.addr == addr)
        });
        let now = Instant::now();
        let mut routes = previous
            .filter(|peer| peer.public == announce.public())
            .map(|peer| peer.routes.clone())
            .unwrap_or_default();
        let previous_route = routes.iter().find(|route| route.addr == addr);
        let previous_auth = previous_route.and_then(|route| route.last_authenticated);
        let source = previous_route.map_or(source, |route| route.source);
        routes.retain(|route| route.addr != addr && route.addr.ip() != source_ip);
        routes.push(PeerEndpoint {
            addr,
            source,
            last_seen: now,
            last_authenticated: previous_auth,
        });
        if routes.len() > MAX_ROUTES_PER_PEER {
            routes.remove(0);
        }
        // Cap enforcement: a NEW peer that would push us over MAX_PEERS evicts the
        // least-recently-seen existing peer first, so a flood of distinct fake ids can
        // churn the table but never grows it past the cap. Known peers (refreshes) are
        // exempt — they overwrite in place and don't add an entry.
        if !existed
            && self.peers.len() >= MAX_PEERS
            && let Some(oldest) = self
                .peers
                .iter()
                .min_by_key(|(_, r)| r.last_seen)
                .map(|(id, _)| id.clone())
        {
            self.peers.remove(&oldest);
            self.announcements.remove(&oldest);
        }
        self.announcements
            .insert(announce.user_id.clone(), announce.clone());
        self.peers.insert(
            announce.user_id.clone(),
            PeerRecord {
                public: announce.public(),
                addr,
                routes,
                name: announce.name.clone(),
                post_office: announce.post_office,
                account_id: announce.account_id(),
                last_seen: now,
            },
        );
        if route_changed {
            self.changes.notify_one();
        }
        if existed {
            UpdateOutcome::Refreshed
        } else {
            UpdateOutcome::New
        }
    }

    pub fn get(&self, user_id: &str) -> Option<&PeerRecord> {
        self.peers.get(user_id)
    }

    /// Record a live address only after a pinned Noise handshake succeeded.
    pub fn mark_authenticated(&mut self, public: &PublicIdentity, addr: SocketAddr) {
        let Some(peer) = self
            .peers
            .get_mut(&public.user_id())
            .filter(|peer| peer.public == *public)
        else {
            return;
        };
        let now = Instant::now();
        if let Some(route) = peer.routes.iter_mut().find(|route| route.addr == addr) {
            route.last_authenticated = Some(now);
            route.last_seen = now;
        } else {
            peer.routes.push(PeerEndpoint {
                addr,
                source: RouteSource::Authenticated,
                last_seen: now,
                last_authenticated: Some(now),
            });
            if peer.routes.len() > MAX_ROUTES_PER_PEER {
                peer.routes.remove(0);
            }
            self.changes.notify_one();
        }
        peer.addr = addr;
        peer.last_seen = now;
    }

    /// Signed source proof retained only while the corresponding roster entry is live.
    pub fn announcement(&self, public: &PublicIdentity) -> Option<&Announce> {
        self.announcements
            .get(&public.user_id())
            .filter(|a| a.public() == *public)
    }

    pub fn announcements(&self) -> Vec<Announce> {
        self.announcements.values().cloned().collect()
    }

    /// Bounded signed identity history; never implies online presence or permission.
    pub(crate) fn historical_announcements(&self) -> Vec<Announce> {
        self.historical
            .iter()
            .filter(|(id, _)| !self.conflicting_history.contains(*id))
            .map(|(_, proof)| proof.clone())
            .collect()
    }

    pub fn peers(&self) -> Vec<PeerRecord> {
        self.peers.values().cloned().collect()
    }

    /// The known peers that advertise the post-office role.
    pub fn post_offices(&self) -> Vec<PeerRecord> {
        self.peers
            .values()
            .filter(|r| r.post_office)
            .cloned()
            .collect()
    }

    /// The public identities of all known devices belonging to `account_id`.
    /// Empty if none are currently known. This is how an account resolves to the
    /// set of endpoints a message must fan out to.
    pub fn devices_of_account(&self, account_id: &str) -> Vec<PublicIdentity> {
        self.peers
            .values()
            .filter(|r| r.account_id.as_deref() == Some(account_id))
            .map(|r| r.public.clone())
            .collect()
    }

    /// Drop peers not seen within `ttl`.
    pub fn evict_stale(&mut self, ttl: Duration) {
        let now = Instant::now();
        self.peers.retain(|_, r| {
            r.routes.retain(|route| {
                let lifespan = match route.source {
                    RouteSource::Lan => ttl,
                    RouteSource::RoutedQuery => ttl.max(Duration::from_secs(120)),
                    RouteSource::Gateway => ttl.max(Duration::from_secs(45)),
                    RouteSource::Authenticated => ttl.max(Duration::from_secs(300)),
                    RouteSource::PostOffice => ttl.max(Duration::from_secs(45)),
                };
                now.saturating_duration_since(route.last_seen) < lifespan
            });
            if let Some(last) = r
                .routes
                .iter()
                .rev()
                .find(|route| route.source != RouteSource::PostOffice)
                .or_else(|| r.routes.last())
            {
                r.addr = last.addr;
                r.last_seen = last.last_seen;
                true
            } else {
                now.saturating_duration_since(r.last_seen) < ttl
            }
        });
        self.announcements
            .retain(|id, _| self.peers.contains_key(id));
    }

    /// Authenticated private presence supersedes an earlier public sighting.
    pub fn forget(&mut self, public: &PublicIdentity) {
        let id = public.user_id();
        if self.peers.get(&id).is_some_and(|p| p.public == *public) {
            self.peers.remove(&id);
            self.announcements.remove(&id);
        }
    }

    /// Withdraw only the mailbox introduction, preserving independently
    /// discovered LAN or routed paths for the same identity.
    pub fn forget_relayed(&mut self, user_id: &str) {
        let Some(peer) = self.peers.get_mut(user_id) else {
            return;
        };
        peer.routes
            .retain(|route| route.source != RouteSource::PostOffice);
        if let Some(route) = peer.routes.last() {
            peer.addr = route.addr;
            peer.last_seen = route.last_seen;
        } else {
            self.peers.remove(user_id);
            self.announcements.remove(user_id);
        }
        self.changes.notify_one();
    }
}

#[cfg(test)]
mod routed_tests {
    use super::*;
    use crate::identity::account::Account;
    use crate::identity::device::DeviceIdentity;

    #[test]
    fn same_identity_keeps_lan_and_routed_addresses() {
        let id = DeviceIdentity::generate();
        let announce = Announce::new(&id, "Bob", 47475);
        let lan: IpAddr = "192.168.1.20".parse().unwrap();
        let routed: IpAddr = "100.101.102.20".parse().unwrap();
        let mut roster = Roster::default();
        roster.update(&announce, lan, "self");
        roster.update_routed(&announce, routed, "self");
        let peer = roster.get(&id.user_id()).unwrap();
        assert_eq!(peer.routes.len(), 2);
        assert!(peer.routes.iter().any(|route| route.addr.ip() == lan));
        assert!(peer.routes.iter().any(|route| route.addr.ip() == routed));
        assert_eq!(peer.addr.ip(), routed);
    }

    #[test]
    fn relay_introduction_keeps_direct_address_and_never_claims_relay_as_peer() {
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &Account::generate(), "Bob", 47475);
        let direct: IpAddr = "192.168.1.20".parse().unwrap();
        let relay: SocketAddr = "100.101.102.30:47800".parse().unwrap();
        let mut roster = Roster::default();
        assert_eq!(
            roster.update_relayed(&proof, relay, "self"),
            UpdateOutcome::New
        );
        assert_eq!(roster.get(&proof.user_id).unwrap().addr, relay);
        roster.update(&proof, direct, "self");
        roster.update_relayed(&proof, relay, "self");
        let peer = roster.get(&proof.user_id).unwrap();
        assert_eq!(peer.addr.ip(), direct);
        assert!(
            peer.routes
                .iter()
                .any(|r| r.source == RouteSource::PostOffice && r.addr == relay)
        );
        roster.forget_relayed(&proof.user_id);
        assert_eq!(roster.get(&proof.user_id).unwrap().addr.ip(), direct);
    }

    #[test]
    fn relay_introduction_rejects_post_office_identity() {
        let device = DeviceIdentity::generate();
        let proof =
            Announce::new_post_office_with_account(&device, &Account::generate(), "Relay", 47475);
        let relay: SocketAddr = "100.101.102.30:47800".parse().unwrap();
        let mut roster = Roster::default();
        assert_eq!(
            roster.update_relayed(&proof, relay, "self"),
            UpdateOutcome::Rejected
        );
        assert!(roster.get(&proof.user_id).is_none());
        assert!(roster.historical_announcement(&device.public()).is_none());
    }

    #[test]
    fn authenticated_connection_marks_the_existing_route_without_changing_its_origin() {
        let id = DeviceIdentity::generate();
        let announce = Announce::new(&id, "Bob", 47475);
        let ip: IpAddr = "100.101.1.20".parse().unwrap();
        let addr = SocketAddr::new(ip, 47475);
        let mut roster = Roster::default();
        roster.update_routed(&announce, ip, "self");
        roster.mark_authenticated(&id.public(), addr);
        let route = &roster.get(&id.user_id()).unwrap().routes[0];
        assert_eq!(route.source, RouteSource::RoutedQuery);
        assert!(route.last_authenticated.is_some());
    }

    #[test]
    fn gateway_route_requires_a_certified_post_office_proof() {
        let id = DeviceIdentity::generate();
        let account = Account::generate();
        let ordinary = Announce::new_with_account(&id, &account, "ordinary", 47475);
        let uncertified = Announce::new_post_office(&id, "relay", 47475);
        let relay = Announce::new_post_office_with_account(&id, &account, "relay", 47475);
        let ip: IpAddr = "100.101.102.30".parse().unwrap();
        let mut roster = Roster::default();
        assert_eq!(
            roster.update_gateway(&ordinary, ip, 47475, "self"),
            UpdateOutcome::Rejected
        );
        assert_eq!(
            roster.update_gateway(&uncertified, ip, 47475, "self"),
            UpdateOutcome::Rejected
        );
        assert_eq!(
            roster.update_gateway(&relay, ip, 50000, "self"),
            UpdateOutcome::New
        );
        let peer = roster.get(&id.user_id()).unwrap();
        assert!(peer.post_office);
        assert_eq!(peer.routes[0].source, RouteSource::Gateway);
        assert_eq!(peer.addr, SocketAddr::new(ip, 50000));
    }

    #[test]
    fn authenticated_new_address_is_a_separate_bounded_route() {
        let id = DeviceIdentity::generate();
        let proof = Announce::new(&id, "Bob", 47475);
        let mut roster = Roster::default();
        roster.update(&proof, "192.168.1.20".parse().unwrap(), "self");
        let new_addr: SocketAddr = "100.101.1.20:47475".parse().unwrap();
        roster.mark_authenticated(&id.public(), new_addr);
        let peer = roster.get(&id.user_id()).unwrap();
        assert_eq!(peer.addr, new_addr);
        assert_eq!(peer.routes.len(), 2);
        let route = peer
            .routes
            .iter()
            .find(|route| route.addr == new_addr)
            .unwrap();
        assert_eq!(route.source, RouteSource::Authenticated);
        assert!(route.last_authenticated.is_some());
        roster.forget_relayed(&id.user_id());
        assert_eq!(roster.get(&id.user_id()).unwrap().routes.len(), 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;
    use std::net::Ipv4Addr;

    fn ip() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50))
    }

    #[tokio::test]
    async fn new_or_relocated_peer_wakes_recovery() {
        let alice = DeviceIdentity::generate();
        let mut roster = Roster::default();
        let changed = roster.changes();
        roster.update(&Announce::new(&alice, "Alice", 4000), ip(), "self");
        tokio::time::timeout(Duration::from_secs(1), changed.notified())
            .await
            .expect("new peer wakes recovery");

        roster.update(&Announce::new(&alice, "Alice", 4001), ip(), "self");
        tokio::time::timeout(Duration::from_secs(1), changed.notified())
            .await
            .expect("changed route wakes recovery");
    }

    #[test]
    fn historical_cursor_skips_conflicted_first_peer_and_wraps() {
        let mut identities = [DeviceIdentity::generate(), DeviceIdentity::generate()];
        identities.sort_by_key(|d| d.public().user_id());
        let account = crate::identity::account::Account::generate();
        let mut roster = Roster::default();
        for device in &identities {
            roster.update(
                &Announce::new_with_account(device, &account, "peer", 9),
                ip(),
                "self",
            );
        }
        roster.update(
            &Announce::new_with_account(
                &identities[0],
                &crate::identity::account::Account::generate(),
                "conflict",
                9,
            ),
            ip(),
            "self",
        );
        let valid = identities[1].public();
        assert_eq!(roster.next_historical_after(None).unwrap().public(), valid);
        assert_eq!(
            roster
                .next_historical_after(Some(&valid.user_id()))
                .unwrap()
                .public(),
            valid
        );
    }

    #[test]
    fn historical_identity_survives_expiry_but_conflicting_account_fails_closed() {
        use crate::identity::account::Account;
        let device = DeviceIdentity::generate();
        let mut roster = Roster::default();
        // An accountless legacy announcement has no binding to conflict with.
        roster.update(&Announce::new(&device, "Alice", 1), ip(), "self");
        let account = Account::generate();
        let first = Announce::new_with_account(&device, &account, "Alice", 1);
        roster.update(&first, ip(), "self");
        roster.evict_stale(Duration::ZERO);
        assert!(roster.peers().is_empty());
        assert_eq!(roster.historical_announcements(), vec![first]);
        let changed = Announce::new_with_account(&device, &Account::generate(), "Alice", 1);
        roster.update(&changed, ip(), "self");
        assert!(roster.historical_announcements().is_empty());
        assert_eq!(roster.announcements(), vec![changed]);
    }

    #[test]
    fn records_a_valid_announce_with_source_ip_and_port() {
        let alice = DeviceIdentity::generate();
        let announce = Announce::new(&alice, "Alice", 4000);
        let mut roster = Roster::default();

        assert_eq!(
            roster.update(&announce, ip(), "some-other-self"),
            UpdateOutcome::New
        );
        let rec = roster
            .get(&alice.public().user_id())
            .expect("alice recorded");
        assert_eq!(rec.addr, SocketAddr::new(ip(), 4000));
        assert_eq!(rec.name, "Alice");
        assert_eq!(rec.public, alice.public());
    }

    #[test]
    fn self_announce_is_filtered_out() {
        let me = DeviceIdentity::generate();
        let announce = Announce::new(&me, "Me", 4000);
        let mut roster = Roster::default();
        assert_eq!(
            roster.update(&announce, ip(), &me.public().user_id()),
            UpdateOutcome::Rejected
        );
        assert!(roster.get(&me.public().user_id()).is_none());
    }

    #[test]
    fn forged_announce_is_rejected() {
        let alice = DeviceIdentity::generate();
        let mut announce = Announce::new(&alice, "Alice", 4000);
        announce.sig[0] ^= 0xFF;
        let mut roster = Roster::default();
        assert_eq!(
            roster.update(&announce, ip(), "self"),
            UpdateOutcome::Rejected
        );
        assert!(roster.get(&alice.public().user_id()).is_none());
    }

    #[test]
    fn peers_lists_all_records() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut roster = Roster::default();
        roster.update(&Announce::new(&alice, "Alice", 4000), ip(), "self");
        roster.update(&Announce::new(&bob, "Bob", 4001), ip(), "self");
        assert_eq!(roster.peers().len(), 2);
    }

    #[test]
    fn evict_stale_drops_old_records() {
        let alice = DeviceIdentity::generate();
        let mut roster = Roster::default();
        roster.update(&Announce::new(&alice, "Alice", 4000), ip(), "self");

        roster.evict_stale(Duration::from_secs(3600));
        assert!(roster.get(&alice.public().user_id()).is_some());
        roster.evict_stale(Duration::ZERO);
        assert!(roster.get(&alice.public().user_id()).is_none());
    }

    #[test]
    fn re_announce_refreshes_addr_for_same_peer() {
        // A peer that re-announces on a new port overwrites its prior record
        // (the addr-binding argument relies on this).
        let alice = DeviceIdentity::generate();
        let mut roster = Roster::default();
        roster.update(&Announce::new(&alice, "Alice", 4000), ip(), "self");
        roster.update(&Announce::new(&alice, "Alice", 5000), ip(), "self");

        let rec = roster
            .get(&alice.public().user_id())
            .expect("alice present");
        assert_eq!(rec.addr.port(), 5000); // refreshed to the new port
        assert_eq!(roster.peers().len(), 1); // still one peer, not two
    }

    #[test]
    fn update_reports_new_then_refreshed_for_same_peer() {
        let alice = DeviceIdentity::generate();
        let mut roster = Roster::default();
        // First sight → New; re-announce of the same peer → Refreshed.
        assert_eq!(
            roster.update(&Announce::new(&alice, "Alice", 4000), ip(), "self"),
            UpdateOutcome::New
        );
        assert_eq!(
            roster.update(&Announce::new(&alice, "Alice", 4001), ip(), "self"),
            UpdateOutcome::Refreshed
        );
    }

    #[test]
    fn get_unknown_user_returns_none() {
        let roster = Roster::default();
        assert!(roster.get("nonexistent-user-id").is_none());
    }

    #[test]
    fn post_offices_lists_only_flagged_peers() {
        let alice = DeviceIdentity::generate();
        let relay = DeviceIdentity::generate();
        let mut roster = Roster::default();
        roster.update(&Announce::new(&alice, "Alice", 4000), ip(), "self");
        roster.update(
            &Announce::new_post_office(&relay, "Relay", 4001),
            ip(),
            "self",
        );

        // Both peers are present; only the relay is a post office.
        assert_eq!(roster.peers().len(), 2);
        let pos = roster.post_offices();
        assert_eq!(pos.len(), 1);
        assert_eq!(pos[0].public, relay.public());
        assert!(pos[0].post_office);
        // The normal peer's record carries the flag too (false).
        assert!(!roster.get(&alice.public().user_id()).unwrap().post_office);
    }

    #[test]
    fn groups_two_devices_under_one_account() {
        use crate::identity::account::Account;
        let account = Account::generate();
        let dev_a = DeviceIdentity::generate();
        let dev_b = DeviceIdentity::generate();
        let mut roster = Roster::default();
        roster.update(
            &Announce::new_with_account(&dev_a, &account, "A", 4000),
            ip(),
            "self",
        );
        roster.update(
            &Announce::new_with_account(&dev_b, &account, "B", 4001),
            ip(),
            "self",
        );

        let devices = roster.devices_of_account(&account.account_id());
        assert_eq!(devices.len(), 2);
        assert!(devices.contains(&dev_a.public()));
        assert!(devices.contains(&dev_b.public()));
    }

    #[test]
    fn does_not_group_devices_of_a_different_account() {
        use crate::identity::account::Account;
        let mine = Account::generate();
        let theirs = Account::generate();
        let my_dev = DeviceIdentity::generate();
        let their_dev = DeviceIdentity::generate();
        let mut roster = Roster::default();
        roster.update(
            &Announce::new_with_account(&my_dev, &mine, "Mine", 4000),
            ip(),
            "self",
        );
        roster.update(
            &Announce::new_with_account(&their_dev, &theirs, "Theirs", 4001),
            ip(),
            "self",
        );

        assert_eq!(roster.devices_of_account(&mine.account_id()).len(), 1);
        assert_eq!(roster.devices_of_account(&theirs.account_id()).len(), 1);
        assert!(roster.devices_of_account("unknown-account").is_empty());
    }

    #[test]
    fn roster_is_bounded_under_a_flood_of_distinct_peers() {
        // Announce far more distinct (validly-signed) peers than the cap; the roster
        // must never exceed MAX_PEERS, evicting the least-recently-seen to make room.
        let mut roster = Roster::default();
        for i in 0..(MAX_PEERS + 50) {
            let dev = DeviceIdentity::generate();
            let outcome =
                roster.update(&Announce::new(&dev, format!("peer{i}"), 4000), ip(), "self");
            assert_eq!(outcome, UpdateOutcome::New);
            assert!(
                roster.peers().len() <= MAX_PEERS,
                "roster grew past the cap at insert {i}"
            );
        }
        assert_eq!(roster.peers().len(), MAX_PEERS);
    }

    #[test]
    fn refreshing_a_known_peer_is_not_blocked_by_the_cap() {
        // Fill to the cap, then re-announce an EXISTING peer: it must refresh in place
        // (no new entry, no eviction churn).
        let mut roster = Roster::default();
        let first = DeviceIdentity::generate();
        roster.update(&Announce::new(&first, "first", 4000), ip(), "self");
        for i in 1..MAX_PEERS {
            let dev = DeviceIdentity::generate();
            roster.update(&Announce::new(&dev, format!("p{i}"), 4000), ip(), "self");
        }
        assert_eq!(roster.peers().len(), MAX_PEERS);
        // The first peer is still present and re-announcing it refreshes (not New).
        let outcome = roster.update(&Announce::new(&first, "first", 4001), ip(), "self");
        assert_eq!(outcome, UpdateOutcome::Refreshed);
        assert_eq!(roster.peers().len(), MAX_PEERS);
        assert_eq!(
            roster.get(&first.public().user_id()).unwrap().addr.port(),
            4001
        );
    }

    #[test]
    fn peer_without_account_is_not_grouped() {
        let dev = DeviceIdentity::generate();
        let mut roster = Roster::default();
        roster.update(&Announce::new(&dev, "Solo", 4000), ip(), "self");
        let rec = roster.get(&dev.public().user_id()).expect("recorded");
        assert_eq!(rec.account_id, None);
    }
}
