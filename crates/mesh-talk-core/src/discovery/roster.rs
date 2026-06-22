//! The roster: the set of currently-known peers, keyed by `user_id`.

use crate::discovery::announce::Announce;
use crate::identity::device::PublicIdentity;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

/// A peer's stable fingerprint id.
pub type UserId = String;

/// What we know about a discovered peer: its keys, where to reach it, whether it
/// serves as a post office, and when we last heard from it.
#[derive(Debug, Clone)]
pub struct PeerRecord {
    pub public: PublicIdentity,
    pub addr: SocketAddr,
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
}

impl Roster {
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
        if announce.user_id == self_user_id {
            return UpdateOutcome::Rejected; // self-filter
        }
        if !announce.verify() {
            return UpdateOutcome::Rejected;
        }
        let existed = self.peers.contains_key(&announce.user_id);
        // Cap enforcement: a NEW peer that would push us over MAX_PEERS evicts the
        // least-recently-seen existing peer first, so a flood of distinct fake ids can
        // churn the table but never grows it past the cap. Known peers (refreshes) are
        // exempt — they overwrite in place and don't add an entry.
        if !existed && self.peers.len() >= MAX_PEERS {
            if let Some(oldest) = self
                .peers
                .iter()
                .min_by_key(|(_, r)| r.last_seen)
                .map(|(id, _)| id.clone())
            {
                self.peers.remove(&oldest);
            }
        }
        self.peers.insert(
            announce.user_id.clone(),
            PeerRecord {
                public: announce.public(),
                addr: SocketAddr::new(source_ip, announce.tcp_port),
                name: announce.name.clone(),
                post_office: announce.post_office,
                account_id: announce.account_id(),
                last_seen: Instant::now(),
            },
        );
        if existed {
            UpdateOutcome::Refreshed
        } else {
            UpdateOutcome::New
        }
    }

    pub fn get(&self, user_id: &str) -> Option<&PeerRecord> {
        self.peers.get(user_id)
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
        self.peers
            .retain(|_, r| now.duration_since(r.last_seen) < ttl);
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
