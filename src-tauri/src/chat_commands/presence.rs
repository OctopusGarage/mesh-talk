//! Client-relative public presence. Routes and membership never establish freshness.
use super::NodeState;
use crate::commands::CommandError;
use mesh_talk_core::discovery::roster::{PeerRecord, RouteSource};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

#[derive(Serialize, Clone)]
pub struct PresenceEvidence {
    source: &'static str,
    last_seen_secs: u64,
    expires_in_secs: u64,
}

#[derive(Serialize, Clone)]
pub struct PresenceDevice {
    user_id: String,
    evidence: Vec<PresenceEvidence>,
}

#[derive(Serialize, Default)]
pub struct PresenceInfo {
    pub online: bool,
    pub last_seen_secs: Option<u64>,
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

fn add_peer(out: &mut HashMap<String, PresenceInfo>, peer: &PeerRecord, now: Instant) {
    if peer.post_office {
        return;
    }
    let key = peer
        .account_id
        .clone()
        .unwrap_or_else(|| format!("device:{}", peer.public.user_id()));
    let evidence: Vec<_> = peer
        .presence_evidence(now)
        .into_iter()
        .filter_map(|(source, age, remaining)| {
            let source = match source {
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
            })
        })
        .collect();
    // Never manufacture history from the timestamp of a mere route.
    let entry = out.entry(key).or_default();
    if let Some(age) = evidence.iter().map(|e| e.last_seen_secs).min() {
        entry.last_seen_secs = Some(entry.last_seen_secs.map_or(age, |old| old.min(age)));
        entry.online = true;
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
) {
    // Certification can arrive while an unbound cached snapshot is in flight.
    // Only project evidence when the entire optional binding is still current.
    if current_binding != peer.account_id.as_deref() {
        return;
    }
    add_peer(out, peer, now);
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

/// One shared snapshot. No per-member dials, retained connections or transitive gossip.
#[tauri::command]
pub async fn get_presence(
    state: tauri::State<'_, NodeState>,
) -> Result<HashMap<String, PresenceInfo>, CommandError> {
    let node = {
        let guard = state.0.lock().await;
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
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    if !rt.discovery_available() || !std::sync::Arc::ptr_eq(&rt.handle(), &node) {
        return Err(CommandError::not_started());
    }
    let now = Instant::now();
    let mut out = HashMap::new();
    for peer in &peers {
        // Cached snapshots already reject invalid/retired certificates. Resolve the
        // current binding again rather than assigning evidence to an obsolete account.
        add_current_peer(
            &mut out,
            peer,
            rt.account_id_for_device(&peer.public).as_deref(),
            now,
        );
    }
    let own = out.entry(rt.account_id().to_owned()).or_default();
    own.online = true;
    own.last_seen_secs = Some(0);
    own.devices.push(PresenceDevice {
        user_id: rt.user_id().to_owned(),
        evidence: vec![PresenceEvidence {
            source: "self",
            last_seen_secs: 0,
            expires_in_secs: mesh_talk_core::discovery::roster::PUBLIC_PRESENCE_TTL.as_secs(),
        }],
    });
    for person in out.values_mut() {
        person.devices.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        for device in &mut person.devices {
            device
                .evidence
                .sort_by_key(|e| (e.source, e.last_seen_secs, e.expires_in_secs));
        }
    }
    for channel in rt.list_channels() {
        let pending: HashSet<_> = rt.channel_pending_members(channel.id).into_iter().collect();
        let mut accounts = HashSet::new();
        let mut unbound = HashSet::new();
        for device in rt.channel_members(channel.id) {
            let id = device.user_id();
            if pending.contains(&id) || node.device_is_post_office(&device) {
                continue;
            }
            let account = if id == rt.user_id() {
                Some(rt.account_id().to_owned())
            } else {
                rt.account_id_for_device(&device)
            };
            if let Some(account) = account {
                accounts.insert(account);
            } else {
                unbound.insert(id);
            }
        }
        add_group(
            &mut out,
            hex::encode(channel.id.as_bytes()),
            accounts,
            unbound,
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
        add_current_peer(&mut unchanged, &cached, None, now);
        assert!(
            unchanged[&fallback].online,
            "unbound devices remain visible while their binding is unchanged"
        );

        let mut raced = HashMap::new();
        add_current_peer(&mut raced, &cached, Some("newly-certified-account"), now);
        assert!(
            raced.is_empty(),
            "a stale unbound snapshot must not publish a fallback after certification"
        );
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
