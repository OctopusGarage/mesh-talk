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
    /// Signed UDP discovery whose local-versus-routed scope is not established.
    Discovery,
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
    /// Public observation deadline for authenticated/directory evidence, never a route lease.
    pub presence_expires_at: Option<Instant>,
    /// Original directory expiry in Unix seconds, retained across repeated old-relay evidence.
    pub presence_origin_expires_at: Option<u64>,
}

impl PeerEndpoint {
    fn public_presence_deadline(&self) -> Option<Instant> {
        match self.source {
            RouteSource::Lan | RouteSource::Discovery => Some(self.last_seen + PUBLIC_PRESENCE_TTL),
            RouteSource::RoutedQuery => self
                .presence_expires_at
                .or(Some(self.last_seen + PUBLIC_PRESENCE_TTL)),
            RouteSource::Authenticated | RouteSource::PostOffice => self.presence_expires_at,
            RouteSource::Gateway => None,
        }
    }
}

const MAX_ROUTES_PER_PEER: usize = 8;
/// Public observations expire promptly after logout, independently of dial-route retention.
pub const PUBLIC_PRESENCE_TTL: Duration = Duration::from_secs(15);
/// Presentation recovery ends sixty seconds from the original public observation.
pub const PUBLIC_RECONNECT_TTL: Duration = Duration::from_secs(60);

/// Keep a directly discovered endpoint ahead of a bridge gateway, even when
/// the gateway refreshes its directory more recently. Routes within each tier
/// retain their existing newest-first behavior.
fn preferred_route(routes: &[PeerEndpoint]) -> Option<&PeerEndpoint> {
    routes
        .iter()
        .rev()
        .find(|route| {
            route.source != RouteSource::Gateway && route.source != RouteSource::PostOffice
        })
        .or_else(|| {
            routes
                .iter()
                .rev()
                .find(|route| route.source != RouteSource::PostOffice)
        })
        .or_else(|| routes.last())
}

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

impl PeerRecord {
    /// Fresh public observations, independently of preferred route or TCP success.
    /// The third value is the remaining monotonic lifetime in seconds.
    pub fn presence_evidence(&self, now: Instant) -> Vec<(RouteSource, u64, u64)> {
        self.routes
            .iter()
            .filter_map(|route| {
                let age = now.saturating_duration_since(route.last_seen).as_secs();
                let deadline = route.public_presence_deadline()?;
                (deadline > now).then(|| {
                    (
                        route.source,
                        age,
                        deadline.saturating_duration_since(now).as_secs(),
                    )
                })
            })
            .collect()
    }

    /// A bounded recovery hint, never fresh Online evidence. Repeated directory
    /// metadata and private transport activity cannot move this deadline.
    pub fn reconnect_remaining(&self, now: Instant) -> Duration {
        self.routes
            .iter()
            .filter_map(|route| {
                let deadline = route.public_presence_deadline()?;
                Some(if route.source == RouteSource::RoutedQuery {
                    route.last_seen + PUBLIC_RECONNECT_TTL
                } else {
                    deadline + (PUBLIC_RECONNECT_TTL - PUBLIC_PRESENCE_TTL)
                })
            })
            .map(|deadline| deadline.saturating_duration_since(now))
            .max()
            .unwrap_or(Duration::ZERO)
    }

    /// A known application route without an intermediate Mesh Talk post office.
    /// Discovery is a candidate; transport still verifies reachability and identity.
    pub fn has_direct_route(&self) -> bool {
        self.routes.iter().any(|route| {
            matches!(
                route.source,
                RouteSource::Lan
                    | RouteSource::Discovery
                    | RouteSource::RoutedQuery
                    | RouteSource::Authenticated
            )
        })
    }

    /// Whether this peer has a current, directly discovered LAN path.
    pub fn has_lan_route(&self) -> bool {
        self.routes
            .iter()
            .any(|route| route.source == RouteSource::Lan)
    }
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

/// Undo only one staged directory update, retaining independent route changes.
pub(crate) struct RelayedRollback {
    user_id: String,
    public: PublicIdentity,
    previous: Vec<(usize, PeerEndpoint)>,
    staged: PeerEndpoint,
}

/// The known-peers map.
#[derive(Default)]
pub struct Roster {
    peers: HashMap<UserId, PeerRecord>,
    announcements: HashMap<UserId, Announce>,
    historical: BTreeMap<UserId, Announce>,
    conflicting_history: HashSet<UserId>,
    linked_bindings: HashMap<UserId, (PublicIdentity, crate::identity::account::DeviceCertificate)>,
    changes: std::sync::Arc<Notify>,
}

impl Roster {
    /// Install a locally authorized own-account device transition. Callers derive
    /// this from the durable account-device registry, never discovery traffic.
    pub(crate) fn authorize_linked_account(
        &mut self,
        public: &PublicIdentity,
        certificate: &crate::identity::account::DeviceCertificate,
    ) {
        debug_assert!(certificate.verify());
        debug_assert_eq!(certificate.device_ed25519_pub, public.ed25519_pub);
        let id = public.user_id();
        let changed = self
            .announcements
            .get(&id)
            .into_iter()
            .chain(self.historical.get(&id))
            .any(|old| old.public() != *public || old.account_cert.as_ref() != Some(certificate))
            || self.conflicting_history.contains(&id);
        if changed {
            self.forget(public);
            self.historical.remove(&id);
            self.conflicting_history.remove(&id);
        }
        self.linked_bindings
            .insert(id, (public.clone(), certificate.clone()));
    }

    fn linked_binding_matches(&self, announce: &Announce) -> bool {
        self.linked_bindings
            .get(&announce.user_id)
            .is_none_or(|(public, cert)| {
                announce.public() == *public && announce.account_cert.as_ref() == Some(cert)
            })
    }

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

    pub(crate) fn update_unscoped(
        &mut self,
        announce: &Announce,
        ip: IpAddr,
        own: &str,
    ) -> UpdateOutcome {
        self.update_from(announce, ip, announce.tcp_port, own, RouteSource::Discovery)
    }

    /// Public, authenticated presence carries no claim of physical LAN scope.
    pub fn update_authenticated(
        &mut self,
        announce: &Announce,
        ip: IpAddr,
        own: &str,
    ) -> UpdateOutcome {
        self.update_from(
            announce,
            ip,
            announce.tcp_port,
            own,
            RouteSource::Authenticated,
        )
    }

    /// Attach a directory origin deadline only to the exact staged introduction.
    pub(crate) fn set_directory_expiry(
        &mut self,
        id: &str,
        relay: SocketAddr,
        expiry: Instant,
        origin_secs: u64,
    ) {
        if let Some(peer) = self.peers.get_mut(id) {
            for route in &mut peer.routes {
                if route.addr == relay && route.source == RouteSource::PostOffice {
                    let now = Instant::now();
                    let capped = expiry.min(now + PUBLIC_PRESENCE_TTL);
                    // Only a newer origin registration can renew the shorter public
                    // lease. Compare original wire seconds, without conversion jitter.
                    let newer = route
                        .presence_origin_expires_at
                        .is_none_or(|old| origin_secs > old);
                    route.presence_expires_at = Some(if newer {
                        capped
                    } else {
                        route.presence_expires_at.unwrap_or(capped).min(capped)
                    });
                    if newer {
                        route.presence_origin_expires_at = Some(origin_secs);
                    }
                }
            }
        }
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

    /// Only nonce-bound, signature-verified discovery can supply this RTT.
    /// Slow paths get up to sixty seconds; healthy samples shrink by five
    /// seconds per observation, avoiding an abrupt return to a strict lease.
    pub(crate) fn update_routed_with_latency(
        &mut self,
        announce: &Announce,
        source_ip: IpAddr,
        own: &str,
        rtt: Duration,
    ) -> UpdateOutcome {
        let previous = self
            .peers
            .get(&announce.user_id)
            .filter(|p| p.public == announce.public())
            .and_then(|p| {
                p.routes
                    .iter()
                    .find(|r| r.source == RouteSource::RoutedQuery && r.addr.ip() == source_ip)
            })
            .and_then(|r| {
                r.presence_expires_at
                    .map(|deadline| deadline.saturating_duration_since(r.last_seen))
            })
            .unwrap_or(PUBLIC_PRESENCE_TTL);
        let measured = (PUBLIC_PRESENCE_TTL
            + rtt
                .saturating_sub(Duration::from_millis(100))
                .min(Duration::from_secs(5))
                * 150)
            .min(PUBLIC_RECONNECT_TTL);
        let lease = measured
            .max(previous.saturating_sub(Duration::from_secs(5)))
            .max(PUBLIC_PRESENCE_TTL);
        let outcome = self.update_routed(announce, source_ip, own);
        if outcome != UpdateOutcome::Rejected
            && let Some(peer) = self.peers.get_mut(&announce.user_id)
            && let Some(route) = peer
                .routes
                .iter_mut()
                .find(|r| r.source == RouteSource::RoutedQuery && r.addr.ip() == source_ip)
        {
            route.presence_expires_at = Some(route.last_seen + lease);
        }
        outcome
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
            || !announce.valid_presence()
            || !self.linked_binding_matches(announce)
            || announce.post_office
            || announce.account_id().is_none()
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
        let prior = routes
            .iter()
            .find(|route| route.addr == relay && route.source == RouteSource::PostOffice);
        let prior_expiry = prior.and_then(|route| route.presence_expires_at);
        let prior_origin = prior.and_then(|route| route.presence_origin_expires_at);
        routes.retain(|route| route.addr != relay || route.source != RouteSource::PostOffice);
        routes.push(PeerEndpoint {
            addr: relay,
            source: RouteSource::PostOffice,
            last_seen: now,
            last_authenticated: None,
            presence_expires_at: prior_expiry,
            presence_origin_expires_at: prior_origin,
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
        let direct = preferred_route(&routes).map(|route| route.addr);
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

    /// Stage a directory route without evicting an unrelated peer at capacity.
    pub(crate) fn stage_relayed(
        &mut self,
        announce: &Announce,
        relay: SocketAddr,
        self_user_id: &str,
    ) -> Option<RelayedRollback> {
        if !self.peers.contains_key(&announce.user_id) && self.peers.len() >= MAX_PEERS {
            return None;
        }
        let previous = self
            .peers
            .get(&announce.user_id)
            .map(|peer| peer.routes.clone())
            .unwrap_or_default();
        if self.update_relayed(announce, relay, self_user_id) == UpdateOutcome::Rejected {
            return None;
        }
        let staged = self
            .peers
            .get(&announce.user_id)?
            .routes
            .iter()
            .find(|route| route.addr == relay && route.source == RouteSource::PostOffice)?
            .clone();
        // Retain only records this update changed. Restoring the entire old
        // snapshot would resurrect routes independently removed while awaiting IO.
        let current = &self.peers.get(&announce.user_id)?.routes;
        let previous = previous
            .into_iter()
            .enumerate()
            .filter(|(_, old)| {
                !current.iter().any(|route| {
                    route.addr == old.addr
                        && route.source == old.source
                        && route.last_seen == old.last_seen
                        && route.last_authenticated == old.last_authenticated
                })
            })
            .collect();
        Some(RelayedRollback {
            user_id: announce.user_id.clone(),
            public: announce.public(),
            previous,
            staged,
        })
    }

    /// Restore overwritten or capacity-evicted routes without replacing newer
    /// direct/authenticated observations. Revocation must not restore old relay paths.
    pub(crate) fn rollback_relayed(&mut self, undo: RelayedRollback, restore_relays: bool) {
        let Some(peer) = self
            .peers
            .get_mut(&undo.user_id)
            .filter(|peer| peer.public == undo.public)
        else {
            return;
        };
        peer.routes.retain(|route| {
            !(route.addr == undo.staged.addr
                && route.source == undo.staged.source
                && route.last_seen == undo.staged.last_seen
                && route.last_authenticated == undo.staged.last_authenticated)
        });
        for (index, route) in undo.previous {
            if (!restore_relays && route.source == RouteSource::PostOffice)
                || peer
                    .routes
                    .iter()
                    .any(|current| current.addr == route.addr && current.source == route.source)
            {
                continue;
            }
            // A concurrent discovery may have consumed the restored slot. Keep
            // independently learned routes ahead of the oldest mailbox hint.
            if peer.routes.len() == MAX_ROUTES_PER_PEER {
                let oldest_relay = peer
                    .routes
                    .iter()
                    .enumerate()
                    .filter(|(_, current)| current.source == RouteSource::PostOffice)
                    .min_by_key(|(_, current)| current.last_seen)
                    .map(|(index, _)| index);
                if route.source == RouteSource::PostOffice {
                    continue;
                }
                let Some(oldest_relay) = oldest_relay else {
                    continue;
                };
                peer.routes.remove(oldest_relay);
            }
            peer.routes.insert(index.min(peer.routes.len()), route);
        }
        if let Some(route) = preferred_route(&peer.routes) {
            peer.addr = route.addr;
            peer.last_seen = route.last_seen;
        } else {
            self.peers.remove(&undo.user_id);
            self.announcements.remove(&undo.user_id);
        }
        self.changes.notify_one();
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
        if !announce.valid_presence() || !self.linked_binding_matches(announce) {
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
        let previous_route = routes
            .iter()
            .find(|route| route.addr == addr && route.source == source);
        let previous_auth = previous_route.and_then(|route| route.last_authenticated);
        routes.retain(|route| {
            route.source != source || (route.addr != addr && route.addr.ip() != source_ip)
        });
        routes.push(PeerEndpoint {
            addr,
            source,
            last_seen: now,
            last_authenticated: previous_auth,
            presence_expires_at: (source == RouteSource::Authenticated)
                .then_some(now + PUBLIC_PRESENCE_TTL),
            presence_origin_expires_at: None,
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
                addr: preferred_route(&routes).map_or(addr, |route| route.addr),
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
            if route.source == RouteSource::Authenticated {
                route.last_seen = now;
            }
        } else {
            peer.routes.push(PeerEndpoint {
                addr,
                source: RouteSource::Authenticated,
                last_seen: now,
                last_authenticated: Some(now),
                presence_expires_at: None,
                presence_origin_expires_at: None,
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
                    RouteSource::Lan | RouteSource::Discovery => ttl,
                    RouteSource::RoutedQuery => ttl.max(Duration::from_secs(120)),
                    RouteSource::Gateway => ttl.max(Duration::from_secs(45)),
                    RouteSource::Authenticated => ttl.max(Duration::from_secs(300)),
                    RouteSource::PostOffice => ttl.max(Duration::from_secs(45)),
                };
                now.saturating_duration_since(route.last_seen) < lifespan
            });
            if let Some(last) = preferred_route(&r.routes) {
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
        if let Some(route) = preferred_route(&peer.routes) {
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
    fn measured_routed_latency_grows_the_online_lease_and_shrinks_it_gradually() {
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &Account::generate(), "Peer", 1);
        let mut roster = Roster::default();
        let ip = "127.0.0.1".parse().unwrap();
        roster.update_routed_with_latency(&proof, ip, "self", Duration::from_millis(10));
        let route = &roster.get(&proof.user_id).unwrap().routes[0];
        assert_eq!(
            route.presence_expires_at.unwrap() - route.last_seen,
            Duration::from_secs(15)
        );
        roster.update_routed_with_latency(&proof, ip, "self", Duration::from_millis(500));
        let peer = roster.get(&proof.user_id).unwrap();
        let route = &peer.routes[0];
        assert_eq!(
            route.presence_expires_at.unwrap() - route.last_seen,
            Duration::from_secs(60)
        );
        assert!(
            !peer
                .presence_evidence(route.last_seen + Duration::from_secs(45))
                .is_empty()
        );
        roster.update_routed_with_latency(&proof, ip, "self", Duration::from_millis(10));
        let route = &roster.get(&proof.user_id).unwrap().routes[0];
        assert_eq!(
            route.presence_expires_at.unwrap() - route.last_seen,
            Duration::from_secs(55)
        );
        for _ in 0..20 {
            roster.update_routed_with_latency(&proof, ip, "self", Duration::from_millis(10));
        }
        let route = &roster.get(&proof.user_id).unwrap().routes[0];
        assert_eq!(
            route.presence_expires_at.unwrap() - route.last_seen,
            Duration::from_secs(15)
        );
    }

    #[test]
    fn routed_latency_lease_tolerates_loss_without_sliding_beyond_sixty_seconds() {
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &Account::generate(), "Peer", 1);
        let mut roster = Roster::default();
        roster.update_routed(&proof, "127.0.0.1".parse().unwrap(), "self");
        let peer = roster.peers.get_mut(&proof.user_id).unwrap();
        let origin = peer.routes[0].last_seen;
        peer.routes[0].presence_expires_at = Some(origin + Duration::from_secs(60));
        assert!(
            !peer
                .presence_evidence(origin + Duration::from_secs(45))
                .is_empty()
        );
        assert_eq!(
            peer.reconnect_remaining(origin + Duration::from_secs(59)),
            Duration::from_secs(1)
        );
        assert!(
            peer.presence_evidence(origin + Duration::from_secs(60))
                .is_empty()
        );
        assert_eq!(
            peer.reconnect_remaining(origin + Duration::from_secs(60)),
            Duration::ZERO
        );
    }

    #[test]
    fn reconnect_window_is_bounded_by_public_deadline_not_route_refresh() {
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &Account::generate(), "Peer", 1);
        let mut roster = Roster::default();
        roster.update_authenticated(&proof, "127.0.0.1".parse().unwrap(), "self");
        let peer = roster.get(&proof.user_id).unwrap();
        let origin = peer.routes[0].presence_expires_at.unwrap() - PUBLIC_PRESENCE_TTL;
        assert_eq!(peer.reconnect_remaining(origin), Duration::from_secs(60));
        assert!(
            peer.presence_evidence(origin + Duration::from_secs(15))
                .is_empty()
        );
        assert_eq!(
            peer.reconnect_remaining(origin + Duration::from_secs(15)),
            Duration::from_secs(45)
        );
        assert_eq!(
            peer.reconnect_remaining(origin + Duration::from_secs(60)),
            Duration::ZERO
        );
        assert_eq!(
            peer.reconnect_remaining(origin + Duration::from_secs(59)),
            Duration::from_secs(1)
        );
        let mut copy = peer.clone();
        copy.routes[0].last_seen = origin + Duration::from_secs(59);
        copy.routes[0].last_authenticated = Some(copy.routes[0].last_seen);
        assert_eq!(
            copy.reconnect_remaining(origin + Duration::from_secs(60)),
            Duration::ZERO
        );
        copy.routes[0].presence_expires_at = None;
        assert_eq!(copy.reconnect_remaining(origin), Duration::ZERO);
        copy.routes[0].source = RouteSource::Gateway;
        assert_eq!(copy.reconnect_remaining(origin), Duration::ZERO);
        roster.forget(&proof.public());
        assert!(
            roster.get(&proof.user_id).is_none(),
            "explicit withdrawal has no reconnect grace"
        );
    }

    #[test]
    fn logout_expires_authenticated_presence_without_discarding_routes() {
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &Account::generate(), "Peer", 1);
        let mut roster = Roster::default();
        roster.update_authenticated(&proof, "127.0.0.1".parse().unwrap(), "self");
        let observed = Instant::now();
        let peer = roster.get(&proof.user_id).unwrap();
        assert!(!peer.presence_evidence(observed).is_empty());
        assert!(
            peer.presence_evidence(observed + Duration::from_secs(20))
                .is_empty(),
            "a logged-out desktop must not remain online for the route's five-minute lease"
        );
        assert!(peer.has_direct_route());
    }

    #[test]
    fn authenticated_route_success_cannot_manufacture_or_extend_public_presence() {
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &Account::generate(), "Peer", 1);
        let mut roster = Roster::default();
        roster.update_authenticated(&proof, "127.0.0.1".parse().unwrap(), "self");
        let past = Instant::now() - Duration::from_secs(16);
        for route in &mut roster.peers.get_mut(&proof.user_id).unwrap().routes {
            route.last_seen = past;
            route.presence_expires_at = Some(past + PUBLIC_PRESENCE_TTL);
        }
        roster.mark_authenticated(&device.public(), "127.0.0.1:1".parse().unwrap());
        roster.mark_authenticated(&device.public(), "127.0.0.2:1".parse().unwrap());
        let peer = roster.get(&proof.user_id).unwrap();
        assert!(peer.has_direct_route());
        assert!(peer.presence_evidence(Instant::now()).is_empty());
    }

    #[test]
    fn person_presence_observations_expire_at_each_sources_own_deadline() {
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &Account::generate(), "Peer", 1);
        let mut roster = Roster::default();
        roster.update(&proof, "127.0.0.1".parse().unwrap(), "self");
        let template = roster.get(&proof.user_id).unwrap().clone();
        let observed = Instant::now();
        for (source, lifetime) in [
            (RouteSource::Lan, 15),
            (RouteSource::Discovery, 15),
            (RouteSource::RoutedQuery, 15),
            (RouteSource::Authenticated, 15),
            (RouteSource::PostOffice, 7),
        ] {
            let mut peer = template.clone();
            peer.routes[0].source = source;
            peer.routes[0].last_seen = observed;
            peer.routes[0].presence_expires_at = Some(observed + Duration::from_secs(lifetime));
            let deadline = observed + Duration::from_secs(lifetime);
            assert!(
                !peer
                    .presence_evidence(deadline - Duration::from_millis(1))
                    .is_empty(),
                "{source:?} must remain eligible before its deadline"
            );
            assert!(
                peer.presence_evidence(deadline).is_empty(),
                "{source:?} must expire exactly at its own deadline"
            );
            assert!(
                peer.presence_evidence(deadline + Duration::from_secs(1))
                    .is_empty()
            );
        }
    }

    #[test]
    fn old_relay_forty_five_second_origin_gets_one_short_public_lease() {
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &Account::generate(), "Peer", 1);
        let relay = "127.0.0.2:2".parse().unwrap();
        let mut roster = Roster::default();
        roster.update_relayed(&proof, relay, "self");
        let now = Instant::now();
        let origin = now + Duration::from_secs(45);
        roster.set_directory_expiry(&proof.user_id, relay, origin, 45);
        let peer = roster.get(&proof.user_id).unwrap();
        assert!(!peer.presence_evidence(now).is_empty());
        assert!(
            peer.presence_evidence(now + Duration::from_secs(20))
                .is_empty()
        );
        let expired = now - Duration::from_secs(1);
        roster.peers.get_mut(&proof.user_id).unwrap().routes[0].presence_expires_at = Some(expired);
        roster.update_relayed(&proof, relay, "self");
        roster.set_directory_expiry(
            &proof.user_id,
            relay,
            origin + Duration::from_millis(900),
            45,
        );
        assert!(
            roster
                .get(&proof.user_id)
                .unwrap()
                .presence_evidence(Instant::now())
                .is_empty()
        );
        roster.set_directory_expiry(&proof.user_id, relay, origin + Duration::from_secs(5), 50);
        assert!(
            !roster
                .get(&proof.user_id)
                .unwrap()
                .presence_evidence(Instant::now())
                .is_empty()
        );
    }

    #[test]
    fn legacy_directory_refresh_cannot_renew_an_existing_origin_deadline() {
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &Account::generate(), "Peer", 1);
        let relay = "127.0.0.2:2".parse().unwrap();
        let mut roster = Roster::default();
        roster.update_relayed(&proof, relay, "self");
        let deadline = Instant::now() + Duration::from_secs(3);
        roster.set_directory_expiry(&proof.user_id, relay, deadline, 3);
        for _ in 0..3 {
            roster.update_relayed(&proof, relay, "self");
        }
        let peer = roster.get(&proof.user_id).unwrap();
        assert_eq!(peer.routes[0].presence_expires_at, Some(deadline));
        assert!(peer.presence_evidence(deadline).is_empty());
        roster.update_routed(&proof, "100.100.0.1".parse().unwrap(), "self");
        roster.forget_relayed(&proof.user_id);
        let peer = roster.get(&proof.user_id).unwrap();
        assert_eq!(peer.presence_evidence(Instant::now()).len(), 1);
        assert_eq!(
            peer.presence_evidence(Instant::now())[0].0,
            RouteSource::RoutedQuery
        );
    }

    #[test]
    fn directory_expiry_and_authentication_never_renew_discovery() {
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &Account::generate(), "Peer", 1);
        let relay = "127.0.0.2:2".parse().unwrap();
        let ip = "127.0.0.1".parse().unwrap();
        let mut roster = Roster::default();
        roster.update_relayed(&proof, relay, "self");
        let now = Instant::now();
        roster.set_directory_expiry(&proof.user_id, relay, now + Duration::from_secs(3), 3);
        let peer = roster.get(&proof.user_id).unwrap();
        assert_eq!(peer.presence_evidence(now).len(), 1);
        assert!(!peer.has_direct_route());
        assert!(
            peer.presence_evidence(now + Duration::from_secs(3))
                .is_empty()
        );
        roster.update(&proof, ip, "self");
        assert_eq!(
            roster
                .get(&proof.user_id)
                .unwrap()
                .presence_evidence(now + Duration::from_secs(4))
                .len(),
            1
        );
        let seen = roster
            .get(&proof.user_id)
            .unwrap()
            .routes
            .iter()
            .find(|r| r.source == RouteSource::Lan)
            .unwrap()
            .last_seen;
        roster.update_authenticated(&proof, ip, "self");
        roster.mark_authenticated(&device.public(), "127.0.0.1:1".parse().unwrap());
        let peer = roster.get(&proof.user_id).unwrap();
        assert_eq!(
            peer.routes
                .iter()
                .find(|r| r.source == RouteSource::Lan)
                .unwrap()
                .last_seen,
            seen
        );
        assert!(
            peer.presence_evidence(now + Duration::from_secs(14))
                .iter()
                .any(|e| e.0 == RouteSource::Authenticated)
        );
        assert!(
            !peer
                .presence_evidence(now + Duration::from_secs(121))
                .iter()
                .any(|e| e.0 == RouteSource::Lan)
        );
    }

    #[test]
    fn presence_sources_expire_independently_and_routes_do_not_imply_presence() {
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &Account::generate(), "Peer", 1);
        let mut roster = Roster::default();
        roster.update(&proof, "127.0.0.1".parse().unwrap(), "self");
        roster.update_routed(&proof, "100.100.0.1".parse().unwrap(), "self");
        let mut peer = roster.get(&device.user_id()).unwrap().clone();
        let now = Instant::now();
        assert_eq!(peer.presence_evidence(now).len(), 2);
        for route in &mut peer.routes {
            route.last_seen = now
                - Duration::from_secs(if route.source == RouteSource::Lan {
                    16
                } else {
                    10
                });
        }
        assert_eq!(peer.presence_evidence(now).len(), 1);
        assert_eq!(peer.presence_evidence(now)[0].0, RouteSource::RoutedQuery);
        assert!(
            peer.presence_evidence(now + Duration::from_secs(6))
                .is_empty()
        );
        peer.routes.clear();
        assert!(peer.presence_evidence(now).is_empty());
        roster.update_relayed(&proof, "127.0.0.2:2".parse().unwrap(), "self");
        let peer = roster.get(&device.user_id()).unwrap();
        assert!(
            !peer
                .presence_evidence(now)
                .iter()
                .any(|e| e.0 == RouteSource::PostOffice),
            "legacy directory proofs have no origin freshness"
        );
    }

    #[test]
    fn direct_route_capability_distinguishes_transport_from_relay_hints() {
        let id = DeviceIdentity::generate();
        let announce = Announce::new(&id, "Peer", 47475);
        let mut roster = Roster::default();
        roster.update_routed(&announce, "100.101.1.20".parse().unwrap(), "self");
        let mut peer = roster.get(&id.user_id()).unwrap().clone();
        for (source, direct) in [
            (RouteSource::Lan, true),
            (RouteSource::Discovery, true),
            (RouteSource::RoutedQuery, true),
            (RouteSource::Authenticated, true),
            (RouteSource::Gateway, false),
            (RouteSource::PostOffice, false),
        ] {
            peer.routes[0].source = source;
            assert_eq!(peer.has_direct_route(), direct, "{source:?}");
        }
        let mut direct = peer.routes[0].clone();
        direct.source = RouteSource::RoutedQuery;
        peer.routes.push(direct);
        assert!(
            peer.has_direct_route(),
            "an indirect preferred route must not hide an alternative direct route"
        );
        peer.routes.clear();
        assert!(
            !peer.has_direct_route(),
            "an address alone is not direct-route evidence"
        );
    }

    #[test]
    fn authorized_linked_binding_retires_old_beacons_and_historical_conflicts() {
        let device = DeviceIdentity::generate();
        let old = Account::generate();
        let adopted = Account::generate();
        let before = Announce::new_with_account(&device, &old, "Before", 1234);
        let after = Announce::new_with_account(&device, &adopted, "After", 1235);
        let mut roster = Roster::default();
        let ip = "127.0.0.1".parse().unwrap();
        assert!(roster.update(&before, ip, "self").accepted());
        roster.authorize_linked_account(&device.public(), after.account_cert.as_ref().unwrap());
        assert!(roster.get(&device.user_id()).is_none());
        assert!(!roster.update(&before, ip, "self").accepted());
        assert!(roster.update(&after, ip, "self").accepted());
        assert_eq!(
            roster.historical_announcement(&device.public()),
            Some(&after)
        );
        assert!(
            !roster
                .update_relayed(&before, "127.0.0.1:7890".parse().unwrap(), "self")
                .accepted()
        );
        assert_eq!(
            roster.get(&device.user_id()).unwrap().account_id,
            Some(adopted.account_id())
        );
    }

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
    fn direct_discovery_rejects_signed_unusable_endpoints() {
        let id = DeviceIdentity::generate();
        let source: IpAddr = "192.168.1.20".parse().unwrap();
        let zero_port = Announce::new(&id, "Bob", 0);
        let oversized_name = Announce::new(&id, "x".repeat(1025), 47475);
        assert!(zero_port.verify());
        assert!(oversized_name.verify());
        let mut roster = Roster::default();
        for proof in [&zero_port, &oversized_name] {
            assert_eq!(
                roster.update(proof, source, "self"),
                UpdateOutcome::Rejected
            );
            assert_eq!(
                roster.update_routed(proof, source, "self"),
                UpdateOutcome::Rejected
            );
        }
        assert!(roster.get(&id.user_id()).is_none());
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
    fn direct_lan_relay_survives_gateway_directory_refresh() {
        let id = DeviceIdentity::generate();
        let account = Account::generate();
        let relay = Announce::new_post_office_with_account(&id, &account, "relay", 47475);
        let lan: IpAddr = "192.168.3.72".parse().unwrap();
        let gateway: IpAddr = "192.168.3.16".parse().unwrap();
        let direct = SocketAddr::new(lan, 47475);
        let mut roster = Roster::default();
        roster.update_gateway(&relay, gateway, 47476, "self");
        assert!(!roster.get(&id.user_id()).unwrap().has_lan_route());
        roster.update(&relay, lan, "self");
        assert!(roster.get(&id.user_id()).unwrap().has_lan_route());
        roster.update_gateway(&relay, gateway, 47476, "self");
        let peer = roster.get(&id.user_id()).unwrap();
        assert_eq!(peer.addr, direct);
        assert_eq!(peer.routes.len(), 2);
        assert!(
            peer.routes
                .iter()
                .any(|route| route.source == RouteSource::Gateway)
        );
        roster.evict_stale(Duration::from_secs(30));
        assert_eq!(roster.get(&id.user_id()).unwrap().addr, direct);
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
