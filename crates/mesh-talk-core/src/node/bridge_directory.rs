//! Opt-in introductions through an existing post office. The post office keeps
//! only short-lived, self-authored identity proofs; it never speaks for a peer or
//! learns the peer's LAN address from the directory protocol.

use crate::discovery::Announce;
use crate::identity::device::PublicIdentity;
use crate::postoffice::gateway::GatewayProof;
use crate::transport::MAX_PLAINTEXT;
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const REQUEST_V1: &[u8] = b"MTBD1Q";
const REQUEST_V2: &[u8] = b"MTBD2Q";
const REQUEST_V3: &[u8] = b"MTBD3Q";
const REQUEST_V4: &[u8] = b"MTBD4Q";
const RESPONSE_V4: &[u8] = b"MTBD4R";
const RESPONSE: &[u8] = b"MTBD1R";
const TTL: Duration = crate::discovery::roster::PUBLIC_PRESENCE_TTL;
/// Accept the original deployed wire lease while new registrations use the shorter TTL.
const MAX_WIRE_TTL: Duration = Duration::from_secs(45);
pub(crate) const MAX_MEMBERS: usize = 1024;
pub(crate) const LEGACY_MAX_MEMBERS: usize = 128;

fn unix_seconds_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// An opted-in, self-signed client announcement with the origin relay's fixed
/// expiry. Forwarding cannot refresh that expiry after the client disappears.
#[derive(Clone, Serialize, Deserialize)]
pub struct FederatedPresence {
    pub announce: Announce,
    pub expires_at_secs: u64,
}

/// Both clocks are needed: monotonic aging and exact origin identity across re-queries.
#[derive(Clone, Copy)]
pub(crate) struct PresenceDeadline {
    pub expires_at: Instant,
    pub origin_secs: u64,
}

pub type GatewayRoute = (String, u16);
pub type DirectoryPage = (Vec<Announce>, Option<GatewayRoute>, bool);

fn valid_directory_proof(proof: &Announce) -> bool {
    proof.valid_presence() && proof.account_id().is_some()
}

fn valid_gateway_route(proofs: &[Announce], gateway: &(String, u16)) -> bool {
    gateway.1 != 0
        && proofs.iter().any(|proof| {
            proof.post_office && proof.user_id == gateway.0 && valid_directory_proof(proof)
        })
}

pub struct BridgeDirectory {
    members: HashMap<String, (Announce, Instant)>,
    remote_members: HashMap<String, FederatedPresence>,
    gateway: Option<Arc<Mutex<Option<GatewayProof>>>>,
    changes: tokio::sync::watch::Sender<u64>,
}

impl Default for BridgeDirectory {
    fn default() -> Self {
        Self {
            members: HashMap::new(),
            remote_members: HashMap::new(),
            gateway: None,
            changes: tokio::sync::watch::channel(0).0,
        }
    }
}

impl BridgeDirectory {
    pub fn with_gateway(gateway: Arc<Mutex<Option<GatewayProof>>>) -> Self {
        Self {
            members: HashMap::new(),
            remote_members: HashMap::new(),
            gateway: Some(gateway),
            changes: tokio::sync::watch::channel(0).0,
        }
    }

    pub fn subscribe_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// The caller's Noise-authenticated presence is the only admissible proof.
    /// A request body cannot nominate an identity or an address on its behalf.
    pub fn register(
        &mut self,
        public: &PublicIdentity,
        presence: Option<&Announce>,
        publicly_visible: bool,
        now: Instant,
    ) -> Option<Vec<Announce>> {
        self.members
            .retain(|_, (_, seen)| now.saturating_duration_since(*seen) < TTL);
        self.remote_members
            .retain(|_, proof| proof.expires_at_secs > unix_seconds_now());
        let proof = presence.filter(|proof| {
            publicly_visible
                && !proof.post_office
                && valid_directory_proof(proof)
                && proof.public() == *public
        })?;
        if self.members.len() >= MAX_MEMBERS && !self.members.contains_key(&proof.user_id) {
            return None;
        }
        self.members
            .insert(proof.user_id.clone(), (proof.clone(), now));
        // A fresh origin registration renews its lease and must reach other relays.
        self.changes
            .send_modify(|generation| *generation = generation.wrapping_add(1));
        let mut proofs: Vec<_> = self
            .members
            .iter()
            .filter(|(id, _)| *id != &proof.user_id)
            .map(|(_, (proof, _))| proof.clone())
            .collect();
        proofs.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        let mut remote = self
            .remote_members
            .values()
            .filter(|remote| {
                remote.announce.user_id != proof.user_id
                    && !self.members.contains_key(&remote.announce.user_id)
            })
            .map(|remote| remote.announce.clone())
            .collect::<Vec<_>>();
        remote.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        proofs.extend(remote);
        proofs.truncate(MAX_MEMBERS.saturating_sub(1));
        if let Some(gateway) = &self.gateway
            && let Some(remote) = gateway
                .lock()
                .expect("gateway proof lock not poisoned")
                .clone()
            && remote.post_office
            && valid_directory_proof(&remote.announce)
        {
            proofs.push(remote.announce);
        }
        Some(proofs)
    }

    pub fn gateway_route(&self) -> Option<(String, u16)> {
        let proof = self.gateway.as_ref()?.lock().ok()?.clone()?;
        (proof.announce.post_office
            && valid_directory_proof(&proof.announce)
            && proof.proxy_port != 0)
            .then_some((proof.announce.user_id, proof.proxy_port))
    }

    pub fn export_federated(&self, now: Instant, unix_now: u64) -> Vec<FederatedPresence> {
        let mut proofs = Vec::new();
        for (announce, seen) in self.members.values() {
            let age = now.saturating_duration_since(*seen).as_secs();
            if age < TTL.as_secs() && !announce.post_office {
                proofs.push(FederatedPresence {
                    announce: announce.clone(),
                    expires_at_secs: unix_now.saturating_add(TTL.as_secs() - age),
                });
            }
        }
        proofs.sort_by(|a, b| a.announce.user_id.cmp(&b.announce.user_id));
        let mut remote = self
            .remote_members
            .values()
            .filter(|entry| {
                entry.expires_at_secs > unix_now
                    && !self.members.contains_key(&entry.announce.user_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        remote.sort_by(|a, b| a.announce.user_id.cmp(&b.announce.user_id));
        proofs.extend(remote);
        proofs.truncate(MAX_MEMBERS);
        proofs
    }

    pub fn import_federated(&mut self, proofs: &[FederatedPresence], unix_now: u64) {
        self.remote_members
            .retain(|_, proof| proof.expires_at_secs > unix_now);
        for proof in proofs.iter().take(MAX_MEMBERS) {
            if proof.announce.post_office
                || !valid_directory_proof(&proof.announce)
                || proof.expires_at_secs <= unix_now
                || proof.expires_at_secs > unix_now.saturating_add(MAX_WIRE_TTL.as_secs())
                || self.members.contains_key(&proof.announce.user_id)
            {
                continue;
            }
            if self.remote_members.len() >= MAX_MEMBERS
                && !self.remote_members.contains_key(&proof.announce.user_id)
            {
                break;
            }
            let id = &proof.announce.user_id;
            if self
                .remote_members
                .get(id)
                .is_some_and(|old| old.expires_at_secs >= proof.expires_at_secs)
            {
                continue;
            }
            self.remote_members.insert(id.clone(), proof.clone());
            self.changes
                .send_modify(|generation| *generation = generation.wrapping_add(1));
        }
    }
}

pub fn is_request(bytes: &[u8]) -> bool {
    bytes == REQUEST_V1 || bytes == REQUEST_V2 || bytes == REQUEST_V3 || bytes == REQUEST_V4
}

/// Existing directory introductions with original expiry metadata, explicitly negotiated.
pub fn freshness_request() -> &'static [u8] {
    REQUEST_V4
}

impl BridgeDirectory {
    pub(crate) fn encode_snapshot(
        &self,
        request: &[u8],
        proofs: &[Announce],
    ) -> Option<Vec<Vec<u8>>> {
        if request != REQUEST_V4 {
            return encode_for_request(request, proofs, self.gateway_route());
        }
        let now = unix_seconds_now();
        let expiry: HashMap<_, _> = self
            .export_federated(Instant::now(), now)
            .into_iter()
            .map(|p| (p.announce.user_id, p.expires_at_secs))
            .collect();
        let gateway = self.gateway_route();
        encode_pages(proofs, |selected, last| {
            let entries: Vec<_> = selected
                .iter()
                .map(|proof| FederatedPresence {
                    announce: proof.clone(),
                    expires_at_secs: expiry.get(&proof.user_id).copied().unwrap_or(now),
                })
                .collect();
            let mut bytes = Vec::from(RESPONSE_V4);
            bytes.push(u8::from(last));
            bytes.extend_from_slice(
                &bincode::serialize(&(entries, gateway_on_page(selected, gateway.as_ref())))
                    .ok()?,
            );
            (bytes.len() <= MAX_PLAINTEXT).then_some(bytes)
        })
    }
}

pub fn request() -> &'static [u8] {
    REQUEST_V3
}

pub fn gateway_request() -> &'static [u8] {
    REQUEST_V2
}

/// Old decoders admit at most 128 entries across the complete snapshot.
/// Keep their frames unchanged while modern requests explicitly opt into 1024.
pub(crate) fn encode_for_request(
    request: &[u8],
    proofs: &[Announce],
    gateway: Option<GatewayRoute>,
) -> Option<Vec<Vec<u8>>> {
    let limit = if request == REQUEST_V3 {
        MAX_MEMBERS
    } else {
        LEGACY_MAX_MEMBERS
    };
    let mut selected: Vec<_> = proofs
        .iter()
        .filter(|p| !p.post_office)
        .take(limit)
        .cloned()
        .collect();
    let gateway_proof = gateway
        .as_ref()
        .and_then(|(id, _)| proofs.iter().find(|p| p.post_office && p.user_id == *id));
    if let Some(proof) = gateway_proof {
        selected.truncate(limit - 1);
        selected.push(proof.clone());
    }
    if request == REQUEST_V1 {
        encode_response(&selected)
    } else {
        encode_response_with_gateway(&selected, gateway.filter(|_| gateway_proof.is_some()))
    }
}

pub fn legacy_request() -> &'static [u8] {
    REQUEST_V1
}

pub fn encode_response(proofs: &[Announce]) -> Option<Vec<Vec<u8>>> {
    encode_pages(proofs, frame)
}

const RESPONSE_V2: &[u8] = b"MTBD2R";

pub fn encode_response_with_gateway(
    proofs: &[Announce],
    gateway: Option<(String, u16)>,
) -> Option<Vec<Vec<u8>>> {
    if gateway
        .as_ref()
        .is_some_and(|route| !valid_gateway_route(proofs, route))
    {
        return None;
    }
    encode_pages(proofs, |selected, last| {
        frame_v2(selected, gateway_on_page(selected, gateway.as_ref()), last)
    })
}

fn encode_pages(
    proofs: &[Announce],
    frame_page: impl Fn(&[Announce], bool) -> Option<Vec<u8>>,
) -> Option<Vec<Vec<u8>>> {
    let mut ordered: Vec<_> = proofs
        .iter()
        .filter(|proof| valid_directory_proof(proof))
        .cloned()
        .collect();
    ordered.sort_by(|a, b| a.user_id.cmp(&b.user_id));
    let mut pages = Vec::new();
    let mut selected = Vec::new();
    for proof in ordered {
        selected.push(proof);
        if frame_page(&selected, false).is_none() {
            let overflow = selected.pop()?;
            if selected.is_empty() {
                return None;
            }
            pages.push(frame_page(&selected, false)?);
            selected = vec![overflow];
        }
    }
    pages.push(frame_page(&selected, true)?);
    Some(pages)
}

fn gateway_on_page<'a>(
    proofs: &[Announce],
    gateway: Option<&'a (String, u16)>,
) -> Option<&'a (String, u16)> {
    gateway.filter(|(id, _)| proofs.iter().any(|proof| &proof.user_id == id))
}

fn frame_v2(proofs: &[Announce], gateway: Option<&(String, u16)>, last: bool) -> Option<Vec<u8>> {
    let mut bytes = Vec::from(RESPONSE_V2);
    bytes.push(u8::from(last));
    bytes.extend_from_slice(&bincode::serialize(&(proofs, gateway)).ok()?);
    (bytes.len() <= MAX_PLAINTEXT).then_some(bytes)
}

pub fn decode_response_with_gateway(bytes: &[u8]) -> Option<DirectoryPage> {
    let body = bytes.strip_prefix(RESPONSE_V2)?;
    if bytes.len() > MAX_PLAINTEXT || body.is_empty() || body[0] > 1 {
        return None;
    }
    let (proofs, gateway): (Vec<Announce>, Option<(String, u16)>) = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_PLAINTEXT as u64)
        .reject_trailing_bytes()
        .deserialize(&body[1..])
        .ok()?;
    if proofs.len() > MAX_MEMBERS
        || proofs.iter().any(|proof| !valid_directory_proof(proof))
        || gateway
            .as_ref()
            .is_some_and(|route| !valid_gateway_route(&proofs, route))
    {
        return None;
    }
    Some((proofs, gateway, body[0] == 1))
}

fn frame(proofs: &[Announce], last: bool) -> Option<Vec<u8>> {
    let mut bytes = Vec::from(RESPONSE);
    bytes.push(u8::from(last));
    bytes.extend_from_slice(&bincode::serialize(proofs).ok()?);
    (bytes.len() <= MAX_PLAINTEXT).then_some(bytes)
}

pub fn decode_response(bytes: &[u8]) -> Option<(Vec<Announce>, bool)> {
    let body = bytes.strip_prefix(RESPONSE)?;
    if bytes.len() > MAX_PLAINTEXT || body.is_empty() || body[0] > 1 {
        return None;
    }
    let last = body[0] == 1;
    let proofs: Vec<Announce> = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_PLAINTEXT as u64)
        .reject_trailing_bytes()
        .deserialize(&body[1..])
        .ok()?;
    (proofs.len() <= MAX_MEMBERS && proofs.iter().all(valid_directory_proof))
        .then_some((proofs, last))
}

/// A validated, bounded response from one authenticated relay query.
pub(crate) type DirectorySnapshot = (Vec<Announce>, Option<GatewayRoute>);

pub(crate) enum DirectoryProgress {
    More,
    Complete(DirectorySnapshot),
}

pub(crate) struct DirectoryResponse {
    relay_public: PublicIdentity,
    proofs: Vec<Announce>,
    gateway: Option<GatewayRoute>,
    finished: bool,
    pub(crate) expiries: HashMap<String, PresenceDeadline>,
    freshness_version: Option<bool>,
}

impl DirectoryResponse {
    pub(crate) fn new(relay_public: PublicIdentity) -> Self {
        Self {
            relay_public,
            proofs: Vec::new(),
            gateway: None,
            finished: false,
            expiries: HashMap::new(),
            freshness_version: None,
        }
    }

    pub(crate) fn add_page(&mut self, bytes: &[u8]) -> Option<DirectoryProgress> {
        if self.finished {
            return None;
        }
        let fresh = bytes.starts_with(RESPONSE_V4);
        if self.freshness_version.is_some_and(|old| old != fresh) {
            return None;
        }
        self.freshness_version = Some(fresh);
        let decoded = if fresh {
            let body = bytes.strip_prefix(RESPONSE_V4)?;
            if bytes.len() > MAX_PLAINTEXT || body.is_empty() || body[0] > 1 {
                return None;
            }
            let (entries, route): (Vec<FederatedPresence>, Option<GatewayRoute>) =
                bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .with_limit(MAX_PLAINTEXT as u64)
                    .reject_trailing_bytes()
                    .deserialize(&body[1..])
                    .ok()?;
            if entries.len() > MAX_MEMBERS
                || entries.iter().any(|p| !valid_directory_proof(&p.announce))
            {
                return None;
            }
            let now = unix_seconds_now();
            let mut page = Vec::new();
            for entry in entries {
                if !entry.announce.post_office
                    && entry.expires_at_secs > now
                    && entry.expires_at_secs <= now.saturating_add(MAX_WIRE_TTL.as_secs())
                {
                    self.expiries.insert(
                        entry.announce.user_id.clone(),
                        PresenceDeadline {
                            expires_at: Instant::now()
                                + Duration::from_secs(entry.expires_at_secs - now),
                            origin_secs: entry.expires_at_secs,
                        },
                    );
                }
                page.push(entry.announce);
            }
            if route
                .as_ref()
                .is_some_and(|route| !valid_gateway_route(&page, route))
            {
                return None;
            }
            Some((page, route, body[0] == 1))
        } else {
            None
        };
        let (page, route, last) = decoded
            .or_else(|| decode_response_with_gateway(bytes))
            .or_else(|| {
                decode_response(bytes).map(|(page, last)| {
                    let route = page
                        .iter()
                        .find(|proof| proof.post_office && proof.public() != self.relay_public)
                        .map(|proof| (proof.user_id.clone(), proof.tcp_port));
                    (page, route, last)
                })
            })?;
        if self.gateway.is_some() && route.is_some() && self.gateway != route {
            return None;
        }
        self.gateway = route.or(self.gateway.take());
        self.proofs.extend(page);
        if self.proofs.len() > MAX_MEMBERS {
            return None;
        }
        if last {
            self.finished = true;
            Some(DirectoryProgress::Complete((
                std::mem::take(&mut self.proofs),
                self.gateway.take(),
            )))
        } else {
            Some(DirectoryProgress::More)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::account::Account;
    use crate::identity::device::DeviceIdentity;

    #[test]
    fn federated_duplicate_presence_is_quiet_but_newer_origin_wakes_subscribers() {
        let mut directory = BridgeDirectory::default();
        let mut first = directory.subscribe_changes();
        let second = directory.subscribe_changes();
        let now = unix_seconds_now();
        let proof = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "member",
            47475,
        );
        let mut entry = FederatedPresence {
            announce: proof,
            expires_at_secs: now + 10,
        };
        directory.import_federated(&[entry.clone()], now);
        assert!(first.has_changed().unwrap());
        assert!(second.has_changed().unwrap());
        first.borrow_and_update();
        directory.import_federated(&[entry.clone()], now);
        assert!(
            !first.has_changed().unwrap(),
            "equal forwarded leases must not cause federation wake loops"
        );
        entry.expires_at_secs += 1;
        directory.import_federated(&[entry], now);
        assert!(first.has_changed().unwrap());
    }

    #[test]
    fn directory_freshness_rejects_expired_and_out_of_budget_origin_lifetimes() {
        let now = unix_seconds_now();
        let entries: Vec<_> = [now.saturating_sub(1), now, now + 90, now + 7]
            .into_iter()
            .map(|expiry| FederatedPresence {
                announce: Announce::new_with_account(
                    &DeviceIdentity::generate(),
                    &Account::generate(),
                    "Peer",
                    1,
                ),
                expires_at_secs: expiry,
            })
            .collect();
        let valid_id = entries[3].announce.user_id.clone();
        let mut frame = Vec::from(RESPONSE_V4);
        frame.push(1);
        frame.extend_from_slice(
            &bincode::serialize(&(entries, Option::<GatewayRoute>::None)).unwrap(),
        );
        let mut response = DirectoryResponse::new(DeviceIdentity::generate().public());
        assert!(matches!(
            response.add_page(&frame),
            Some(DirectoryProgress::Complete(_))
        ));
        assert_eq!(
            response.expiries.len(),
            1,
            "unusable freshness may supply hints but never online evidence"
        );
        assert!(response.expiries.contains_key(&valid_id));
        assert!(
            response.expiries[&valid_id]
                .expires_at
                .saturating_duration_since(Instant::now())
                <= Duration::from_secs(7)
        );
    }

    #[test]
    fn directory_response_cannot_mix_freshness_and_legacy_pages() {
        let mut frame = Vec::from(RESPONSE_V4);
        frame.push(0);
        frame.extend_from_slice(
            &bincode::serialize(&(
                Vec::<FederatedPresence>::new(),
                Option::<GatewayRoute>::None,
            ))
            .unwrap(),
        );
        let mut response = DirectoryResponse::new(DeviceIdentity::generate().public());
        assert!(matches!(
            response.add_page(&frame),
            Some(DirectoryProgress::More)
        ));
        let legacy = encode_response(&[]).unwrap();
        assert!(response.add_page(&legacy[0]).is_none());
    }

    #[test]
    fn logged_out_directory_member_expires_within_twenty_seconds_across_federation() {
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &Account::generate(), "Messi10", 1);
        let observed = Instant::now();
        let unix_now = unix_seconds_now();
        let mut origin = BridgeDirectory::default();
        origin
            .register(&device.public(), Some(&proof), true, observed)
            .unwrap();
        let exported = origin.export_federated(observed, unix_now);
        assert_eq!(exported[0].expires_at_secs, unix_now + 15);
        let mut relay = BridgeDirectory::default();
        relay.import_federated(&exported, unix_now);
        let forwarded = relay.export_federated(observed + Duration::from_secs(10), unix_now + 10);
        assert_eq!(forwarded[0].expires_at_secs, unix_now + 15);
        relay.import_federated(&forwarded, unix_now + 14);
        assert!(
            relay
                .export_federated(observed + Duration::from_secs(20), unix_now + 20)
                .is_empty()
        );
        assert!(
            origin
                .export_federated(observed + Duration::from_secs(20), unix_now + 20)
                .is_empty()
        );
    }

    #[test]
    fn deployed_forty_five_second_directory_proof_still_decodes_and_forwards() {
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &Account::generate(), "Legacy", 1);
        let now = unix_seconds_now();
        let origin = now + 45;
        let mut directory = BridgeDirectory::default();
        directory.import_federated(
            &[FederatedPresence {
                announce: proof.clone(),
                expires_at_secs: origin,
            }],
            now,
        );
        assert_eq!(
            directory.export_federated(Instant::now(), now)[0].expires_at_secs,
            origin
        );
        let pages = directory
            .encode_snapshot(freshness_request(), &[proof])
            .unwrap();
        let mut response = DirectoryResponse::new(DeviceIdentity::generate().public());
        for page in pages {
            assert!(response.add_page(&page).is_some());
        }
        assert_eq!(response.expiries.len(), 1);
    }

    #[test]
    fn freshness_response_preserves_forwarded_origin_deadline() {
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &Account::generate(), "remote", 1);
        let now = unix_seconds_now();
        let deadline = now + 3;
        let mut directory = BridgeDirectory::default();
        directory.import_federated(
            &[FederatedPresence {
                announce: proof.clone(),
                expires_at_secs: deadline,
            }],
            now,
        );
        for _ in 0..2 {
            let pages = directory
                .encode_snapshot(freshness_request(), std::slice::from_ref(&proof))
                .unwrap();
            let mut response = DirectoryResponse::new(DeviceIdentity::generate().public());
            for page in pages {
                assert!(response.add_page(&page).is_some());
            }
            assert!(
                response.expiries[&proof.user_id]
                    .expires_at
                    .saturating_duration_since(Instant::now())
                    <= Duration::from_secs(3)
            );
            assert_eq!(
                directory.export_federated(Instant::now(), now)[0].expires_at_secs,
                deadline
            );
        }
        let pages = directory
            .encode_snapshot(request(), std::slice::from_ref(&proof))
            .unwrap();
        let mut legacy = DirectoryResponse::new(DeviceIdentity::generate().public());
        for page in pages {
            assert!(legacy.add_page(&page).is_some());
        }
        assert!(
            legacy.expiries.is_empty(),
            "legacy introductions cannot prove origin freshness"
        );
        directory.import_federated(&[], deadline);
        assert!(
            directory
                .export_federated(Instant::now(), deadline)
                .is_empty()
        );
    }

    #[test]
    fn legacy_directory_snapshots_keep_their_original_bound() {
        let account = Account::generate();
        let proofs: Vec<_> = (0..140)
            .map(|_| {
                Announce::new_with_account(&DeviceIdentity::generate(), &account, "member", 47475)
            })
            .collect();
        let relay = DeviceIdentity::generate();
        for request in [legacy_request(), gateway_request()] {
            let mut response = DirectoryResponse::new(relay.public());
            let mut count = None;
            for page in encode_for_request(request, &proofs, None).unwrap() {
                if let DirectoryProgress::Complete((members, _)) = response.add_page(&page).unwrap()
                {
                    count = Some(members.len());
                }
            }
            assert_eq!(count, Some(LEGACY_MAX_MEMBERS));
        }
    }

    #[test]
    fn modern_directory_registers_and_returns_more_than_128_members() {
        let account = Account::generate();
        let mut directory = BridgeDirectory::default();
        let now = Instant::now();
        let identities: Vec<_> = (0..130).map(|_| DeviceIdentity::generate()).collect();
        let mut snapshot = Vec::new();
        for identity in &identities {
            let proof = Announce::new_with_account(identity, &account, "member", 47475);
            snapshot = directory
                .register(&identity.public(), Some(&proof), true, now)
                .expect("a modern directory must not reject its 129th member");
        }
        assert_eq!(snapshot.len(), 129);
        let relay = DeviceIdentity::generate();
        let mut response = DirectoryResponse::new(relay.public());
        let mut complete = false;
        for page in encode_response_with_gateway(&snapshot, None).unwrap() {
            match response
                .add_page(&page)
                .expect("modern snapshot must decode beyond 128 entries")
            {
                DirectoryProgress::More => {}
                DirectoryProgress::Complete((members, _)) => {
                    assert_eq!(members.len(), 129);
                    complete = true;
                }
            }
        }
        assert!(complete);
    }

    #[test]
    fn scalable_request_is_explicitly_versioned() {
        assert!(is_request(b"MTBD3Q"));
        assert_eq!(request(), b"MTBD3Q");
    }

    #[test]
    fn response_assembly_rejects_conflicting_gateway_routes_across_pages() {
        let relay = DeviceIdentity::generate();
        let first_gateway = DeviceIdentity::generate();
        let second_gateway = DeviceIdentity::generate();
        let account = Account::generate();
        let first_proof =
            Announce::new_post_office_with_account(&first_gateway, &account, "first", 47475);
        let second_proof =
            Announce::new_post_office_with_account(&second_gateway, &account, "second", 47476);
        let first_route = (first_gateway.user_id(), 50001);
        let second_route = (second_gateway.user_id(), 50002);
        let first_page = frame_v2(&[first_proof], Some(&first_route), false).unwrap();
        let second_page = frame_v2(&[second_proof], Some(&second_route), true).unwrap();
        let mut response = DirectoryResponse::new(relay.public());

        assert!(matches!(
            response.add_page(&first_page),
            Some(DirectoryProgress::More)
        ));
        assert!(response.add_page(&second_page).is_none());
    }

    #[test]
    fn directory_requests_distinguish_legacy_and_gateway_aware_clients() {
        assert!(is_request(b"MTBD1Q"));
        assert!(is_request(b"MTBD2Q"));
        assert_eq!(legacy_request(), b"MTBD1Q");
        assert_eq!(gateway_request(), b"MTBD2Q");
    }

    #[test]
    fn only_authenticated_visible_self_presence_can_join() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = Announce::new_with_account(&a, &account, "A", 47475);
        let now = Instant::now();
        let mut directory = BridgeDirectory::default();
        assert!(
            directory
                .register(&b.public(), Some(&proof), true, now)
                .is_none()
        );
        assert!(
            directory
                .register(&a.public(), Some(&proof), false, now)
                .is_none()
        );
        assert_eq!(
            directory.register(&a.public(), Some(&proof), true, now),
            Some(Vec::new())
        );
        assert_eq!(
            directory
                .register(
                    &b.public(),
                    Some(&Announce::new_with_account(&b, &account, "B", 47476)),
                    true,
                    now
                )
                .unwrap(),
            vec![proof.clone()]
        );
        assert!(
            directory
                .register(&b.public(), None, true, now + TTL)
                .is_none()
        );
        assert!(directory.members.is_empty());
    }

    #[test]
    fn federated_presence_expires_and_excludes_relay_identities() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let relay = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = Announce::new_with_account(&alice, &account, "Alice", 47475);
        let relay_proof = Announce::new_post_office_with_account(&relay, &account, "Relay", 47476);
        let now = Instant::now();
        let unix_now = unix_seconds_now();
        let mut origin = BridgeDirectory::default();
        origin
            .register(&alice.public(), Some(&proof), true, now)
            .unwrap();
        let mut remote = BridgeDirectory::default();
        let snapshot = origin.export_federated(now, unix_now);
        assert_eq!(snapshot.len(), 1);
        remote.import_federated(&snapshot, unix_now);
        remote.import_federated(
            &[FederatedPresence {
                announce: relay_proof,
                expires_at_secs: unix_now + TTL.as_secs(),
            }],
            unix_now,
        );
        let bob_proof = Announce::new_with_account(&bob, &account, "Bob", 47477);
        let visible = remote
            .register(&bob.public(), Some(&bob_proof), true, now)
            .unwrap();
        assert_eq!(visible, vec![proof.clone()]);
        assert!(
            remote
                .export_federated(now, unix_now + 46)
                .iter()
                .all(|entry| entry.announce.user_id != proof.user_id)
        );
    }

    #[test]
    fn post_office_cannot_register_as_a_directory_member() {
        let account = Account::generate();
        let relay = DeviceIdentity::generate();
        let proof = Announce::new_post_office_with_account(&relay, &account, "Relay", 47475);
        assert!(
            BridgeDirectory::default()
                .register(&relay.public(), Some(&proof), true, Instant::now())
                .is_none()
        );
    }

    #[test]
    fn federation_export_keeps_local_members_when_remote_capacity_is_full() {
        let now = Instant::now();
        let unix_now = unix_seconds_now();
        let account = Account::generate();
        let mut directory = BridgeDirectory::default();
        for index in 0..MAX_MEMBERS {
            let identity = DeviceIdentity::generate();
            let announce =
                Announce::new_with_account(&identity, &account, format!("Remote {index}"), 47475);
            directory.remote_members.insert(
                announce.user_id.clone(),
                FederatedPresence {
                    announce,
                    expires_at_secs: unix_now + TTL.as_secs(),
                },
            );
        }
        let local = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&local, &account, "Local", 47476);
        directory
            .register(&local.public(), Some(&proof), true, now)
            .unwrap();
        let exported = directory.export_federated(now, unix_now);
        assert_eq!(exported.len(), MAX_MEMBERS);
        assert!(
            exported
                .iter()
                .any(|entry| entry.announce.user_id == proof.user_id)
        );
    }

    #[test]
    fn response_rejects_tampered_or_trailing_proofs() {
        let a = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&a, &Account::generate(), "A", 47475);
        let encoded = encode_response(std::slice::from_ref(&proof)).unwrap();
        assert_eq!(encoded.len(), 1);
        assert_eq!(decode_response(&encoded[0]), Some((vec![proof], true)));
        let mut trailing = encoded[0].clone();
        trailing.push(0);
        assert!(decode_response(&trailing).is_none());
    }

    #[test]
    fn encoder_omits_proofs_that_the_decoder_cannot_accept() {
        let identity = DeviceIdentity::generate();
        let unbound = Announce::new(&identity, "unbound", 47475);
        let encoded = encode_response(std::slice::from_ref(&unbound)).unwrap();
        assert_eq!(decode_response(&encoded[0]), Some((vec![], true)));
        let hub = Announce::new_post_office(&identity, "unbound hub", 47475);
        assert!(encode_response_with_gateway(&[hub], Some((identity.user_id(), 50000))).is_none());
    }

    #[test]
    fn version_two_response_binds_gateway_port_to_a_signed_post_office_proof() {
        let identity = DeviceIdentity::generate();
        let proof =
            Announce::new_post_office_with_account(&identity, &Account::generate(), "hub", 47475);
        let route = (identity.user_id(), 50000);
        let pages = encode_response_with_gateway(std::slice::from_ref(&proof), Some(route.clone()))
            .unwrap();
        assert_eq!(
            decode_response_with_gateway(&pages[0]),
            Some((vec![proof.clone()], Some(route), true))
        );
        assert!(encode_response_with_gateway(&[proof], Some(("a".repeat(32), 50000))).is_none());
    }

    #[test]
    fn paged_response_places_gateway_port_with_its_signed_proof() {
        let account = Account::generate();
        let mut proofs: Vec<_> = (0..80)
            .map(|_| {
                Announce::new_with_account(
                    &DeviceIdentity::generate(),
                    &account,
                    "long name".repeat(100),
                    47475,
                )
            })
            .collect();
        let hub = DeviceIdentity::generate();
        let route = (hub.user_id(), 50000);
        proofs.push(Announce::new_post_office_with_account(
            &hub, &account, "hub", 47476,
        ));
        let pages = encode_response_with_gateway(&proofs, Some(route.clone())).unwrap();
        assert!(pages.len() > 1);
        let mut found = Vec::new();
        for page in pages {
            let (entries, gateway, _) = decode_response_with_gateway(&page).unwrap();
            if let Some(gateway) = gateway {
                assert_eq!(gateway, route);
                assert!(entries.iter().any(|entry| entry.user_id == hub.user_id()));
                found.push(gateway);
            }
        }
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn gateway_proof_is_shared_only_after_visible_self_registration() {
        let a = DeviceIdentity::generate();
        let e = DeviceIdentity::generate();
        let account = Account::generate();
        let a_proof = Announce::new_with_account(&a, &account, "A", 47475);
        let remote = Announce::new_post_office_with_account(&e, &account, "E", 47476);
        let gateway = Arc::new(Mutex::new(Some(GatewayProof {
            announce: remote.clone(),
            proxy_port: 50000,
        })));
        let mut directory = BridgeDirectory::with_gateway(Arc::clone(&gateway));
        let now = Instant::now();

        assert!(directory.register(&a.public(), None, true, now).is_none());
        assert!(
            directory
                .register(&a.public(), Some(&a_proof), false, now)
                .is_none()
        );
        assert_eq!(
            directory.register(&a.public(), Some(&a_proof), true, now),
            Some(vec![remote])
        );
        *gateway.lock().unwrap() = None;
        assert_eq!(
            directory.register(&a.public(), Some(&a_proof), true, now),
            Some(Vec::new())
        );
    }

    #[test]
    fn a_full_directory_is_paged_without_losing_identities() {
        let account = Account::generate();
        let proofs: Vec<_> = (0..MAX_MEMBERS)
            .map(|_| {
                Announce::new_with_account(
                    &DeviceIdentity::generate(),
                    &account,
                    "long name".repeat(100),
                    47475,
                )
            })
            .collect();
        let pages = encode_response(&proofs).unwrap();
        assert!(pages.len() > 1);
        let mut decoded = Vec::new();
        for (i, page) in pages.iter().enumerate() {
            let (members, last) = decode_response(page).unwrap();
            assert_eq!(last, i == pages.len() - 1);
            decoded.extend(members);
        }
        decoded.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        let mut expected = proofs;
        expected.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        assert_eq!(decoded, expected);
    }
}
