use super::NodeState;
use crate::commands::CommandError;
use serde::Serialize;

// --- Diagnostics / discovery ------------------------------------------------

/// A discovered peer as shown on the Diagnostics page.
#[derive(Serialize)]
pub struct DiagPeerInfo {
    pub user_id: String,
    pub name: String,
    pub ip: String,
    pub tcp_port: u16,
    pub post_office: bool,
    pub account_id: Option<String>,
    /// Whole seconds since this peer was last heard from.
    pub last_seen_secs: u64,
}

/// This device's own identity + network facts, for the Diagnostics page.
#[derive(Serialize)]
pub struct DiagNetworkInfo {
    pub own_user_id: String,
    pub own_name: String,
    pub account_id: String,
    pub listen_tcp_port: u16,
    pub discovery_port: u16,
    pub multicast_group: String,
    pub interfaces: Vec<String>,
}

/// Snapshot the current roster for the Diagnostics page. Polled by the frontend.
#[tauri::command]
pub async fn diag_get_peers(
    state: tauri::State<'_, NodeState>,
) -> Result<Vec<DiagPeerInfo>, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt
        .peers()
        .into_iter()
        .map(|p| DiagPeerInfo {
            user_id: p.public.user_id(),
            name: p.name,
            ip: p.addr.ip().to_string(),
            tcp_port: p.addr.port(),
            post_office: p.post_office,
            account_id: p.account_id,
            last_seen_secs: p.last_seen.elapsed().as_secs(),
        })
        .collect())
}

/// Force an immediate re-announce + /24 rescan (the manual "announce now" control on
/// the Diagnostics page). Helps converge first-contact when LAN discovery is flaky.
#[tauri::command]
pub async fn rescan_peers(state: tauri::State<'_, NodeState>) -> Result<(), CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    rt.trigger_discovery();
    Ok(())
}

/// This device's own identity + LAN/discovery facts, for the Diagnostics page.
#[tauri::command]
pub async fn diag_network_info(
    state: tauri::State<'_, NodeState>,
) -> Result<DiagNetworkInfo, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(DiagNetworkInfo {
        own_user_id: rt.user_id().to_string(),
        own_name: rt.display_name().to_string(),
        account_id: rt.account_id().to_string(),
        listen_tcp_port: rt.listen_tcp_port(),
        discovery_port: mesh_talk_core::node::DEFAULT_DISCOVERY_PORT,
        multicast_group: mesh_talk_core::node::DISCOVERY_MULTICAST_GROUP.to_string(),
        interfaces: mesh_talk_core::node::ipv4_interface_addrs()
            .into_iter()
            .map(|ip| ip.to_string())
            .collect(),
    })
}

// --- Presence (online / last-seen) ------------------------------------------

/// A peer is considered "online" if heard from within this window. Generous enough
/// to ride out a missed announce tick, tight enough that a departed peer dims promptly.
pub(super) const PRESENCE_TTL_SECS: u64 = 30;

/// Per-conversation presence, keyed by account id (DMs) and channel id (channels).
#[derive(Serialize)]
pub struct PresenceInfo {
    /// True when at least one relevant device was heard from within the TTL.
    pub online: bool,
    /// Whole seconds since the most-recently-seen relevant device (None if never seen).
    pub last_seen_secs: Option<u64>,
}

/// Fold per-device "seconds since last seen" values into a [`PresenceInfo`]: the freshest
/// (minimum) device wins, and the conversation is online if that freshest sighting is
/// strictly within [`PRESENCE_TTL_SECS`]. An empty iterator yields offline / never-seen.
pub(super) fn presence_from_seen(secs: impl Iterator<Item = u64>) -> PresenceInfo {
    let best = secs.min();
    PresenceInfo {
        online: best.is_some_and(|s| s < PRESENCE_TTL_SECS),
        last_seen_secs: best,
    }
}

/// A snapshot of presence for every account + channel conversation, keyed by id.
///
/// Online = the account (DM) has ≥1 device currently in the roster seen within the TTL;
/// for a channel, ≥1 known member device is present within the TTL. Reuses the same
/// roster the diagnostics commands read, so it's a cheap, lock-once snapshot. Polled by
/// the frontend on a slow interval into an isolated store (presence ticks must not
/// re-render the message list).
#[tauri::command]
pub async fn get_presence(
    state: tauri::State<'_, NodeState>,
) -> Result<std::collections::HashMap<String, PresenceInfo>, CommandError> {
    use std::collections::HashMap;
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;

    // last_seen (whole secs) per user_id, from one roster snapshot.
    let peers = rt.peers();
    let seen_by_user: HashMap<String, u64> = peers
        .iter()
        .map(|p| (p.public.user_id(), p.last_seen.elapsed().as_secs()))
        .collect();

    let mut out: HashMap<String, PresenceInfo> = HashMap::new();

    // Per-account presence: the freshest of the account's known devices.
    let mut by_account: HashMap<String, Vec<u64>> = HashMap::new();
    for p in &peers {
        if let Some(acct) = &p.account_id {
            by_account
                .entry(acct.clone())
                .or_default()
                .push(p.last_seen.elapsed().as_secs());
        }
    }
    for (acct, secs) in by_account {
        out.insert(acct, presence_from_seen(secs.into_iter()));
    }

    // Per-channel presence: the freshest of any known member device currently in roster.
    for c in rt.list_channels() {
        let secs: Vec<u64> = rt
            .channel_members(c.id)
            .into_iter()
            .filter_map(|m| seen_by_user.get(&m.user_id()).copied())
            .collect();
        out.insert(
            hex::encode(c.id.as_bytes()),
            presence_from_seen(secs.into_iter()),
        );
    }

    Ok(out)
}
