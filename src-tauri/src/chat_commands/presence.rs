//! Client-relative public presence. Routes and membership never establish freshness.
use super::NodeState;
use crate::commands::CommandError;
use mesh_talk_core::discovery::roster::{PeerRecord, RouteSource};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Serialize, Clone)]
pub struct PresenceEvidence {
    source: &'static str,
    last_seen_secs: u64,
    expires_in_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin_hops: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    post_office: Option<PresencePostOffice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    registration_origin: Option<PresenceRegistrationOrigin>,
}

/// Consented registration relationship, independently expiring from its route.
#[derive(Serialize, Clone)]
pub struct PresenceRegistrationOrigin {
    user_id: String,
    name: String,
    expires_in_secs: f64,
}

/// Immediate directory introducer, not a geographic origin or delivery receipt.
#[derive(Serialize, Clone)]
pub struct PresencePostOffice {
    endpoint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct PresenceDevice {
    user_id: String,
    evidence: Vec<PresenceEvidence>,
}

#[derive(Serialize, Default)]
pub struct PresenceInfo {
    pub online: bool,
    pub connection_weak: bool,
    /// Remaining local/cross-network presentation lease; not fresh device evidence.
    pub online_expires_in_secs: u64,
    pub last_seen_secs: Option<u64>,
    /// Nonrenewing presentation hint; does not count as fresh presence.
    pub reconnect_expires_in_secs: u64,
    devices: Vec<PresenceDevice>,
    /// Accepted distinct certified accounts. Unresolved devices are separate.
    #[serde(skip_serializing_if = "Option::is_none")]
    member_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    online_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unbound_member_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    member_keys: Option<Vec<String>>,
}

fn post_office_sources(peers: &[PeerRecord]) -> HashMap<SocketAddr, PresencePostOffice> {
    let mut sources: HashMap<SocketAddr, PresencePostOffice> = HashMap::new();
    for peer in peers.iter().filter(|peer| peer.post_office) {
        for addr in std::iter::once(peer.addr).chain(peer.routes.iter().map(|route| route.addr)) {
            let id = peer.public.user_id();
            let source = sources.entry(addr).or_insert_with(|| PresencePostOffice {
                endpoint: addr.to_string(),
                user_id: Some(id.clone()),
                name: Some(peer.name.clone()),
            });
            if source.user_id.as_deref() != Some(id.as_str()) {
                // Shared gateway endpoints cannot uniquely identify an office.
                source.user_id = None;
                source.name = None;
            }
        }
    }
    sources
}

#[cfg(test)]
fn add_peer(out: &mut HashMap<String, PresenceInfo>, peer: &PeerRecord, now: Instant) {
    add_peer_with_sources(out, peer, now, &HashMap::new());
}

fn add_peer_with_sources(
    out: &mut HashMap<String, PresenceInfo>,
    peer: &PeerRecord,
    now: Instant,
    offices: &HashMap<SocketAddr, PresencePostOffice>,
) {
    if peer.post_office {
        return;
    }
    let key = peer
        .account_id
        .clone()
        .unwrap_or_else(|| format!("device:{}", peer.public.user_id()));
    let wall_now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let unix_now = wall_now.as_secs();
    let evidence: Vec<_> = peer
        .presence_routes(now)
        .filter_map(|(route, age, remaining)| {
            let source = match route.source {
                RouteSource::Lan => "local",
                RouteSource::Discovery => "unscoped",
                RouteSource::RoutedQuery => "routed",
                RouteSource::PostOffice => "directory",
                RouteSource::Authenticated => "authenticated",
                RouteSource::Gateway => return None,
            };
            Some(PresenceEvidence {
                source,
                last_seen_secs: age,
                expires_in_secs: remaining,
                registration_origin: route
                    .registration_origin
                    .as_ref()
                    .filter(|origin| {
                        route.source == RouteSource::PostOffice
                            && origin.valid_for(&peer.public, unix_now)
                    })
                    .map(|origin| PresenceRegistrationOrigin {
                        user_id: origin.office_user_id(),
                        name: origin.office_name().to_owned(),
                        expires_in_secs: std::time::Duration::from_secs(origin.expires_at_secs())
                            .saturating_sub(wall_now)
                            .as_secs_f64(),
                    }),
                origin_hops: route.origin_hops,
                post_office: (route.source == RouteSource::PostOffice).then(|| {
                    offices
                        .get(&route.addr)
                        .cloned()
                        .unwrap_or_else(|| PresencePostOffice {
                            endpoint: route.addr.to_string(),
                            user_id: None,
                            name: None,
                        })
                }),
            })
        })
        .collect();
    // Never manufacture history from the timestamp of a mere route.
    let entry = out.entry(key).or_default();
    let remaining = peer.reconnect_remaining(now).as_secs();
    entry.reconnect_expires_in_secs = entry.reconnect_expires_in_secs.max(remaining);
    entry.online_expires_in_secs = entry.online_expires_in_secs.max(remaining);
    if let Some(age) = peer.public_observation_age(now).map(|age| age.as_secs()) {
        entry.last_seen_secs = Some(entry.last_seen_secs.map_or(age, |old| old.min(age)));
    }
    let fresh = !evidence.is_empty();
    if remaining > 0 || fresh {
        // A generous adaptive lease is a loss-tolerance budget, not a latency
        // measurement. Fresh observations on any path remain healthy; only
        // delayed observations warrant the quality hint while retaining Online.
        let weak = !fresh
            || evidence.iter().all(|e| {
                e.last_seen_secs >= mesh_talk_core::discovery::roster::PUBLIC_PRESENCE_TTL.as_secs()
            });
        entry.connection_weak = weak && (!entry.online || entry.connection_weak);
        entry.online = true;
    }
    if fresh {
        entry.devices.push(PresenceDevice {
            user_id: peer.public.user_id(),
            evidence,
        });
    }
}

fn add_current_peer(
    out: &mut HashMap<String, PresenceInfo>,
    peer: &PeerRecord,
    current_binding: Option<&str>,
    now: Instant,
    offices: &HashMap<SocketAddr, PresencePostOffice>,
) {
    // Certification can arrive while an unbound cached snapshot is in flight.
    // Only project evidence when the entire optional binding is still current.
    if current_binding != peer.account_id.as_deref() {
        return;
    }
    add_peer_with_sources(out, peer, now, offices);
}

fn add_group(
    out: &mut HashMap<String, PresenceInfo>,
    id: String,
    accounts: HashSet<String>,
    unbound: HashSet<String>,
) {
    let online_count = accounts
        .iter()
        .filter(|key| out.get(*key).is_some_and(|p| p.online))
        .count();
    let mut member_keys: Vec<_> = accounts.into_iter().collect();
    member_keys.sort();
    out.insert(
        id,
        PresenceInfo {
            member_count: Some(member_keys.len()),
            online_count: Some(online_count),
            unbound_member_count: Some(unbound.len()),
            member_keys: Some(member_keys),
            ..Default::default()
        },
    );
}

fn add_self(out: &mut HashMap<String, PresenceInfo>, account_id: String, user_id: String) {
    let lease = mesh_talk_core::discovery::roster::PUBLIC_PRESENCE_TTL.as_secs();
    let own = out.entry(account_id).or_default();
    own.online = true;
    own.connection_weak = false;
    own.online_expires_in_secs = own.online_expires_in_secs.max(lease);
    own.reconnect_expires_in_secs = own.reconnect_expires_in_secs.max(lease);
    own.last_seen_secs = Some(0);
    own.devices.push(PresenceDevice {
        user_id,
        evidence: vec![PresenceEvidence {
            source: "self",
            last_seen_secs: 0,
            expires_in_secs: lease,
            origin_hops: None,
            post_office: None,
            registration_origin: None,
        }],
    });
}

/// One shared snapshot. No per-member dials, retained connections or transitive gossip.
#[tauri::command]
pub async fn get_presence(
    state: tauri::State<'_, NodeState>,
) -> Result<HashMap<String, PresenceInfo>, CommandError> {
    let node = {
        let guard = state.0.read().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        if !rt.discovery_available() {
            return Err(CommandError::Internal("discovery unavailable".into()));
        }
        rt.handle()
    };
    let peers = node
        .cached_peer_snapshot_async()
        .await
        .map_err(|e| CommandError::Io(e.to_string()))?;
    let guard = state.0.read().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    if !rt.discovery_available() || !std::sync::Arc::ptr_eq(&rt.handle(), &node) {
        return Err(CommandError::not_started());
    }
    let now = Instant::now();
    let mut out = HashMap::new();
    let offices = post_office_sources(&peers);
    for peer in &peers {
        // Cached snapshots already reject invalid/retired certificates. Resolve the
        // current binding again rather than assigning evidence to an obsolete account.
        add_current_peer(
            &mut out,
            peer,
            rt.account_id_for_device(&peer.public).as_deref(),
            now,
            &offices,
        );
    }
    add_self(
        &mut out,
        rt.account_id().to_owned(),
        rt.user_id().to_owned(),
    );
    for person in out.values_mut() {
        person.devices.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        for device in &mut person.devices {
            device.evidence.sort_by(|a, b| {
                (
                    a.source,
                    a.post_office.as_ref().map(|office| &office.endpoint),
                )
                    .cmp(&(
                        b.source,
                        b.post_office.as_ref().map(|office| &office.endpoint),
                    ))
            });
        }
    }
    for channel in rt.list_channels()? {
        let (accounts, unbound) = node.channel_member_account_sets(channel.id)?;
        add_group(
            &mut out,
            hex::encode(channel.id.as_bytes()),
            accounts.into_iter().collect(),
            unbound.into_iter().collect(),
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_talk_core::{
        discovery::{Announce, Roster},
        identity::{account::Account, device::DeviceIdentity},
    };

    #[test]
    fn directory_registration_origin_keeps_windows_identity_without_borrowing_introducer_or_lease()
    {
        use mesh_talk_core::node::{OriginConsent, RegistrationOrigin};
        let device = DeviceIdentity::generate();
        let office = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &Account::generate(), "Duncan", 1);
        let unix_now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let origin = RegistrationOrigin::new(
            &office,
            "Windows 10 office",
            OriginConsent::new(&device, &office.public(), unix_now + 5),
        )
        .unwrap();
        let mut roster = Roster::default();
        roster.update_relayed(&proof, "127.0.0.1:47479".parse().unwrap(), "self");
        let mut peer = roster.get(&proof.user_id).unwrap().clone();
        let now = Instant::now();
        peer.routes[0].presence_expires_at = Some(now + std::time::Duration::from_secs(10));
        peer.routes[0].registration_origin = Some(origin.clone());
        let mut out = HashMap::new();
        add_peer(&mut out, &peer, now);
        let json = serde_json::to_value(&out[&proof.account_id().unwrap()]).unwrap();
        let evidence = &json["devices"][0]["evidence"][0];
        assert_eq!(evidence["registration_origin"]["name"], "Windows 10 office");
        assert_eq!(evidence["registration_origin"]["user_id"], office.user_id());
        assert!(
            evidence["registration_origin"]["expires_in_secs"]
                .as_f64()
                .unwrap()
                <= 5.0
        );
        assert_eq!(evidence["post_office"]["endpoint"], "127.0.0.1:47479");
        assert!(evidence["post_office"].get("name").is_none());

        // A valid signature for Duncan cannot establish an origin for someone else.
        peer.public = DeviceIdentity::generate().public();
        let mut out = HashMap::new();
        add_peer(&mut out, &peer, now);
        let json = serde_json::to_value(&out[&proof.account_id().unwrap()]).unwrap();
        assert!(
            json["devices"][0]["evidence"][0]
                .get("registration_origin")
                .is_none()
        );

        // A non-directory route must never expose this optional directory proof.
        peer.public = device.public();
        peer.routes[0].source = RouteSource::Lan;
        let mut out = HashMap::new();
        add_peer(&mut out, &peer, now);
        let json = serde_json::to_value(&out[&proof.account_id().unwrap()]).unwrap();
        assert!(
            json["devices"][0]["evidence"][0]
                .get("registration_origin")
                .is_none()
        );

        peer.routes[0].source = RouteSource::PostOffice;
        peer.routes[0].registration_origin = RegistrationOrigin::new(
            &office,
            "Windows 10 office",
            OriginConsent::new(&device, &office.public(), unix_now.saturating_sub(1)),
        );
        let mut out = HashMap::new();
        add_peer(&mut out, &peer, now);
        let json = serde_json::to_value(&out[&proof.account_id().unwrap()]).unwrap();
        assert!(
            json["devices"][0]["evidence"][0]
                .get("registration_origin")
                .is_none(),
            "a live directory route cannot extend the registration proof"
        );
    }
    #[test]
    fn local_account_has_a_real_presentation_lease_for_frontend_group_aging() {
        let mut out = HashMap::new();
        add_self(
            &mut out,
            "self-account".to_owned(),
            "self-device".to_owned(),
        );
        let own = &out["self-account"];
        assert!(own.online);
        assert!(!own.connection_weak);
        assert_eq!(own.online_expires_in_secs, 15);
        assert_eq!(own.reconnect_expires_in_secs, 15);
        assert_eq!(own.devices[0].evidence[0].source, "self");
        assert_eq!(own.devices[0].evidence[0].expires_in_secs, 15);
        add_group(
            &mut out,
            "group".to_owned(),
            HashSet::from(["self-account".to_owned()]),
            HashSet::new(),
        );
        assert_eq!(out["group"].online_count, Some(1));
    }

    #[test]
    fn directory_sources_keep_exact_office_names_and_expire_independently() {
        let account = Account::generate();
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &account, "Peer", 1);
        let mut roster = Roster::default();
        roster.update(&proof, "192.168.1.2".parse().unwrap(), "self");
        let mut peer = roster.get(&proof.user_id).unwrap().clone();
        let now = peer.routes[0].last_seen;
        let mut offices = Vec::new();
        for (index, name, lifetime) in [
            (3, "mac-mini-post-office", 5),
            (4, "mac2015-post-office", 10),
        ] {
            let office = DeviceIdentity::generate();
            let announce =
                Announce::new_post_office_with_account(&office, &Account::generate(), name, 47479);
            roster.update(
                &announce,
                format!("192.168.1.{index}").parse().unwrap(),
                "self",
            );
            let office = roster.get(&announce.user_id).unwrap().clone();
            let mut route = peer.routes[0].clone();
            route.source = RouteSource::PostOffice;
            route.addr = office.addr;
            route.presence_expires_at = Some(now + std::time::Duration::from_secs(lifetime));
            route.origin_hops = Some(index - 3);
            peer.routes.push(route);
            offices.push(office);
        }
        let mut out = HashMap::new();
        add_peer_with_sources(&mut out, &peer, now, &post_office_sources(&offices));
        let json = serde_json::to_value(&out[&account.account_id()]).unwrap();
        let evidence = json["devices"][0]["evidence"].as_array().unwrap();
        assert_eq!(evidence.len(), 3);
        assert!(evidence[0].get("origin_hops").is_none());
        assert_eq!(evidence[1]["origin_hops"], 0);
        assert_eq!(evidence[2]["origin_hops"], 1);
        assert!(evidence[0].get("post_office").is_none());
        assert_eq!(evidence[1]["post_office"]["name"], "mac-mini-post-office");
        assert_eq!(
            evidence[2]["post_office"]["user_id"],
            offices[1].public.user_id()
        );

        let mut out = HashMap::new();
        add_peer_with_sources(
            &mut out,
            &peer,
            now + std::time::Duration::from_secs(5),
            &post_office_sources(&offices),
        );
        assert_eq!(out[&account.account_id()].devices[0].evidence.len(), 2);
        // A shared/gateway address is insufficient to choose between identities.
        offices[1].addr = offices[0].addr;
        offices[1].routes = offices[0].routes.clone();
        let mut out = HashMap::new();
        add_peer_with_sources(&mut out, &peer, now, &post_office_sources(&offices));
        let json = serde_json::to_value(&out[&account.account_id()]).unwrap();
        assert_eq!(
            json["devices"][0]["evidence"][1]["post_office"]["endpoint"],
            "192.168.1.3:47479"
        );
        assert!(
            json["devices"][0]["evidence"][1]["post_office"]
                .get("name")
                .is_none()
        );
    }

    #[test]
    fn cross_lan_presence_tolerates_two_minutes_but_local_expires_at_fifteen_seconds() {
        let account = Account::generate();
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &account, "Peer", 1);
        for routed in [false, true] {
            let mut roster = Roster::default();
            if routed {
                roster.update_routed(&proof, "100.64.0.2".parse().unwrap(), "self");
            } else {
                roster.update(&proof, "192.168.1.2".parse().unwrap(), "self");
            }
            let peer = roster.get(&proof.user_id).unwrap();
            let origin = peer.routes[0].last_seen;
            let deadline = if routed { 120 } else { 15 };
            for age in [0, 14, 15, 60, 119, 120] {
                let mut out = HashMap::new();
                add_peer(&mut out, peer, origin + std::time::Duration::from_secs(age));
                let p = &out[&account.account_id()];
                assert_eq!(p.online, age < deadline, "routed={routed} age={age}");
                assert_eq!(
                    p.last_seen_secs,
                    Some(age),
                    "history retains the original observation"
                );
                if routed && (15..120).contains(&age) {
                    assert!(p.connection_weak);
                    assert!(
                        p.devices.is_empty(),
                        "tolerance is not fresh device evidence"
                    );
                }
            }
        }
    }

    #[test]
    fn adaptive_lease_is_healthy_while_public_observations_keep_renewing() {
        let account = Account::generate();
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &account, "Peer", 1);
        let mut roster = Roster::default();
        roster.update_routed(&proof, "127.0.0.1".parse().unwrap(), "self");
        let mut peer = roster.get(&proof.user_id).unwrap().clone();
        let origin = peer.routes[0].last_seen;
        peer.routes[0].presence_expires_at = Some(origin + std::time::Duration::from_secs(60));
        let mut initial = HashMap::new();
        add_peer(&mut initial, &peer, origin);
        assert!(initial[&account.account_id()].online);
        assert!(
            !initial[&account.account_id()].connection_weak,
            "an adaptive timeout is not evidence of a currently slow connection"
        );
        for age in [14, 15] {
            let mut out = HashMap::new();
            add_peer(
                &mut out,
                &peer,
                origin + std::time::Duration::from_secs(age),
            );
            let p = &out[&account.account_id()];
            assert!(p.online);
            assert_eq!(p.connection_weak, age >= 15);
        }
        let mut out = HashMap::new();
        add_peer(&mut out, &peer, origin + std::time::Duration::from_secs(45));
        let p = &out[&account.account_id()];
        assert!(p.online);
        assert!(p.connection_weak);
        assert_eq!(p.reconnect_expires_in_secs, 75);
    }

    #[test]
    fn presence_rejects_a_cached_unbound_device_after_account_binding_arrives() {
        let device = DeviceIdentity::generate();
        let announce = Announce::new(&device, "Legacy peer", 1);
        let mut roster = Roster::default();
        roster.update(&announce, "127.0.0.2".parse().unwrap(), "self");
        let cached = roster.get(&device.user_id()).unwrap().clone();
        assert!(cached.account_id.is_none());
        let now = Instant::now();
        let fallback = format!("device:{}", device.user_id());

        let mut unchanged = HashMap::new();
        add_current_peer(&mut unchanged, &cached, None, now, &HashMap::new());
        assert!(
            unchanged[&fallback].online,
            "unbound devices remain visible while their binding is unchanged"
        );

        let mut raced = HashMap::new();
        add_current_peer(
            &mut raced,
            &cached,
            Some("newly-certified-account"),
            now,
            &HashMap::new(),
        );
        assert!(
            raced.is_empty(),
            "a stale unbound snapshot must not publish a fallback after certification"
        );
    }

    #[test]
    fn expired_public_observation_projects_recovery_without_online_or_a_sliding_deadline() {
        let account = Account::generate();
        let device = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&device, &account, "Peer", 1);
        let mut roster = Roster::default();
        roster.update(&proof, "127.0.0.1".parse().unwrap(), "self");
        let peer = roster.get(&proof.user_id).unwrap();
        let observed = peer.routes[0].last_seen;
        for (age, online, remaining) in [
            (0, true, 15),
            (15, false, 0),
            (30, false, 0),
            (59, false, 0),
            (60, false, 0),
        ] {
            let mut out = HashMap::new();
            add_peer(
                &mut out,
                peer,
                observed + std::time::Duration::from_secs(age),
            );
            let p = &out[&account.account_id()];
            assert_eq!(p.online, online);
            assert_eq!(p.reconnect_expires_in_secs, remaining);
        }
        let mut private = peer.clone();
        private.routes[0].source = RouteSource::Authenticated;
        private.routes[0].presence_expires_at = None;
        let mut out = HashMap::new();
        add_peer(&mut out, &private, observed);
        assert!(!out[&account.account_id()].online);
        assert_eq!(out[&account.account_id()].reconnect_expires_in_secs, 0);
    }

    #[test]
    fn presence_stale_devices_and_infrastructure_cannot_override_fresh_people() {
        let account = Account::generate();
        let mut out = HashMap::new();
        for (index, age, office) in [(1, 0, false), (2, 400, false), (3, 0, true)] {
            let device = DeviceIdentity::generate();
            let proof = if office {
                Announce::new_post_office_with_account(&device, &account, "Peer", 1)
            } else {
                Announce::new_with_account(&device, &account, "Peer", 1)
            };
            let mut roster = Roster::default();
            roster.update(&proof, format!("127.0.0.{index}").parse().unwrap(), "self");
            let mut peer = roster.get(&proof.user_id).unwrap().clone();
            for route in &mut peer.routes {
                route.last_seen -= std::time::Duration::from_secs(age);
            }
            add_peer(&mut out, &peer, Instant::now());
        }
        let person = &out[&account.account_id()];
        assert!(person.online);
        assert_eq!(person.devices.len(), 1);
        assert_eq!(person.devices[0].evidence.len(), 1);
        add_group(
            &mut out,
            "empty".into(),
            HashSet::from(["never-observed".into()]),
            HashSet::new(),
        );
        assert_eq!(out["empty"].online_count, Some(0));
    }

    #[test]
    fn presence_deduplicates_people_and_paths_and_does_not_gossip() {
        let account = Account::generate();
        let mut roster = Roster::default();
        for (index, age) in [(1, 0), (2, 10)] {
            let device = DeviceIdentity::generate();
            let proof = Announce::new_with_account(&device, &account, "same name", 1);
            roster.update(&proof, format!("127.0.0.{index}").parse().unwrap(), "self");
            roster.update_routed(
                &proof,
                format!("100.100.0.{index}").parse().unwrap(),
                "self",
            );
            let mut peer = roster.get(&device.user_id()).unwrap().clone();
            for route in &mut peer.routes {
                route.last_seen -= std::time::Duration::from_secs(age);
            }
            let mut out = HashMap::new();
            add_peer(&mut out, &peer, Instant::now());
            assert!(out[&account.account_id()].online);
        }
        let mut out = HashMap::new();
        for peer in roster.peers() {
            add_peer(&mut out, &peer, Instant::now());
        }
        assert_eq!(out[&account.account_id()].devices.len(), 2);
        assert_eq!(out[&account.account_id()].devices[0].evidence.len(), 2);
        add_group(
            &mut out,
            "group".into(),
            HashSet::from([
                account.account_id(),
                account.account_id(),
                "unobserved-C".into(),
            ]),
            HashSet::from(["legacy".into()]),
        );
        assert_eq!(out["group"].member_count, Some(2));
        assert_eq!(out["group"].online_count, Some(1));
        assert_eq!(out["group"].unbound_member_count, Some(1));
        assert!(!out.contains_key("unobserved-C"));
    }
}
