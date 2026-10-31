//! Channel lifecycle: create, membership, sender-key distribution, and sends. Split out of node.rs (one `impl Node` block per domain).

pub(in crate::node) mod account_invitation;
pub(in crate::node) mod account_self_control;
pub(in crate::node) mod owner_control;
mod simple_control;
pub(in crate::node) use simple_control::{SimpleControl, normalize_simple_name};

use super::channel::{ChannelBook, ChannelMembershipProjection};
use super::node::now_millis;
use super::*;
use crate::channel::{ChannelMeta, ChannelState};
use crate::discovery::roster::PeerRecord;
use crate::eventlog::LogError;
use crate::eventlog::event::{ConversationId, EventId, EventKind};
use crate::identity::device::PublicIdentity;
use crate::node::channel::seal_keys_for;
use crate::node::session::{request_durable_have, request_round_paged as request_round};
use futures_util::future::join_all;
use futures_util::stream::{self, StreamExt};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// Limit one background recovery tick even when routes are stale or a peer stalls.
const CHANNEL_RECOVERY_PEERS_PER_TICK: usize = 3;
const CHANNEL_RECOVERY_PEER_TIMEOUT: Duration = Duration::from_secs(15);
const CHANNEL_RECOVERY_INTERVAL: Duration = Duration::from_secs(3);
const CHANNEL_RECOVERY_WAKE_BATCH: usize = 8;
const CHANNEL_RECOVERY_MAX_INTERVAL: Duration = Duration::from_secs(60);
const CHANNEL_RECOVERY_GROUP_CONCURRENCY: usize = 2;

#[derive(Default)]
struct ChannelRecoveryBackoff {
    idle_rounds: u32,
}
impl ChannelRecoveryBackoff {
    fn reset(&mut self) {
        self.idle_rounds = 0;
    }
    fn next_delay(&mut self, jitter: u8) -> Duration {
        let millis =
            CHANNEL_RECOVERY_INTERVAL.as_millis() as u64 * (1u64 << self.idle_rounds.min(5));
        self.idle_rounds = self.idle_rounds.saturating_add(1);
        Duration::from_millis(
            (millis + millis * u64::from(jitter) / 1024)
                .min(CHANNEL_RECOVERY_MAX_INTERVAL.as_millis() as u64),
        )
    }
}
const SMALL_CHANNEL_MEMBER_LIMIT: usize = 5;
const LARGE_CHANNEL_INITIAL_FANOUT: usize = 2;
const CHANNEL_CONTROL_FANOUT_CONCURRENCY: usize = 4;
const CHANNEL_CUSTODY_SCAN_PER_TICK: usize = 128;
const CHANNEL_CUSTODY_RECHECK_TICKS: usize = 20;

/// Members in canonical order: sorted by `user_id` and de-duplicated. `ChannelMeta` encodes
/// the membership Vec positionally, and the same-epoch convergence tie-break in
/// `ChannelState::apply_meta` compares those encodings — so every node MUST order an
/// identical member SET identically, or two concurrent same-epoch membership changes could
/// encode differently and the tie-break would pick different winners on different nodes
/// (divergence). Canonicalizing at every construction site keeps the wire form deterministic.
fn canonical_members(mut members: Vec<PublicIdentity>) -> Vec<PublicIdentity> {
    members.sort_by_key(|m| m.user_id());
    members.dedup_by(|a, b| a.user_id() == b.user_id());
    members
}

fn select_channel_targets(
    mut peers: Vec<PeerRecord>,
    member_count: usize,
    start: usize,
    fanout: usize,
) -> Vec<PeerRecord> {
    peers.sort_by_key(|peer| peer.public.user_id());
    if member_count <= SMALL_CHANNEL_MEMBER_LIMIT || peers.len() <= fanout {
        return peers;
    }
    let peer_count = peers.len();
    peers.rotate_left(start % peer_count);
    peers.truncate(fanout);
    peers
}

fn advance_custody_cursor(
    channel: ConversationId,
    key: (u64, EventId),
    after: &mut HashMap<ConversationId, (u64, EventId)>,
    cycles: &mut HashMap<ConversationId, usize>,
) -> usize {
    let previous = after.insert(channel, key);
    let cycle = cycles.entry(channel).or_default();
    if previous.is_some_and(|old| key <= old) {
        *cycle = cycle.wrapping_add(1);
    }
    *cycle
}

fn next_custody_retry_key(
    candidates: &mut impl Iterator<Item = (u64, EventId)>,
    channel: ConversationId,
    after: &mut HashMap<ConversationId, (u64, EventId)>,
    cycles: &mut HashMap<ConversationId, usize>,
) -> Option<(EventId, usize)> {
    let key = candidates.next()?;
    // Commit progress before an attempt: a failed sync cannot pin the next
    // tick to this message and starve later messages in the same batch.
    let cycle = advance_custody_cursor(channel, key, after, cycles);
    Some((key.1, cycle))
}

fn select_custody_peer(mut peers: Vec<PeerRecord>, cycle: usize) -> Option<PeerRecord> {
    peers.sort_unstable_by_key(|peer| peer.public.ed25519_pub);
    let len = peers.len();
    (len > 0).then(|| peers.swap_remove(cycle % len))
}

/// Local bounded recovery observations. A successful peer exchange never proves
/// that offline members have no additional events.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChannelRecovery {
    pub phase: &'static str,
    pub checked_at: Option<u64>,
}

impl Node {
    /// Historical membership belonging to an account other than the active one.
    pub fn channel_previous_account(
        &self,
        channel: ConversationId,
    ) -> Result<Option<String>, LogError> {
        let projection = self.account_group_projection(channel)?;
        let Some(state) = projection.state.as_ref() else {
            return Ok(None);
        };
        let public = self.identity.public();
        if projection.authority.current_event().is_none() || !state.members().contains(&public) {
            return Ok(None);
        }
        let Some(previous) = state.member_account_id(&public.user_id()) else {
            return Ok(None);
        };
        if previous == self.account_id() {
            return Ok(None);
        }
        // A current account grant or invitation may name another certified
        // device. Its account authority takes precedence over the old tuple.
        if projection
            .authority
            .account_grant(&self.account.public().ed25519_pub)
            .is_some()
        {
            return Ok(None);
        }
        Ok(Some(previous.to_owned()))
    }

    /// A historical device grant does not authorize a newly adopted account.
    pub(in crate::node) fn channel_identity_matches_account(&self, state: &ChannelState) -> bool {
        let public = self.identity.public();
        state.members().contains(&public)
            && (state.owner_account_id().is_none()
                || state.member_account_id(&public.user_id()) == Some(self.account_id().as_str()))
    }
    pub(in crate::node) fn channel_identity_is_current(
        &self,
        state: &ChannelState,
    ) -> Result<bool, LogError> {
        let current = self.account_group_projection(state.id())?;
        Ok(self.channel_identity_is_current_in(state, &current))
    }

    pub(in crate::node) fn channel_identity_is_current_in(
        &self,
        state: &ChannelState,
        current: &ChannelMembershipProjection,
    ) -> bool {
        if !state.is_accepted_member(&self.user_id())
            || !state.members().contains(&self.identity.public())
        {
            return false;
        }
        if current.authority.current_event().is_some() {
            current.authority.key_epoch() == Some(state.epoch())
                && current
                    .authority
                    .is_member(&self.identity.public(), &self.own_group_proof())
        } else {
            state.owner_account_id().is_none()
                && current.state.as_ref().is_some_and(|fresh| {
                    fresh.epoch() == state.epoch()
                        && fresh.members() == state.members()
                        && fresh.owner() == state.owner()
                        && fresh.is_accepted_member(&self.user_id())
                        && !fresh.is_dissolved()
                })
        }
    }

    pub(in crate::node) fn channel_identity_can_manage(
        &self,
        state: &ChannelState,
    ) -> Result<bool, LogError> {
        let current = self.account_group_projection(state.id())?;
        Ok(self.channel_identity_can_manage_in(state, &current))
    }

    fn channel_identity_can_manage_in(
        &self,
        state: &ChannelState,
        current: &ChannelMembershipProjection,
    ) -> bool {
        if !self.channel_identity_is_current_in(state, current) {
            return false;
        }
        if current.authority.current_event().is_some() {
            current
                .authority
                .can_admin(&self.identity.public(), &self.own_group_proof())
        } else {
            state.can_admin(&self.user_id())
        }
    }

    pub(in crate::node) fn project_channel_member_accounts(
        &self,
        members: &[PublicIdentity],
    ) -> (
        std::collections::BTreeSet<String>,
        std::collections::BTreeSet<String>,
    ) {
        let mut accounts = std::collections::BTreeSet::new();
        let mut unbound = std::collections::BTreeSet::new();
        for member in members {
            if self.device_is_post_office(member) {
                continue;
            }
            if let Some(account) = self.account_id_for_device(member) {
                accounts.insert(account);
            } else {
                unbound.insert(member.user_id());
            }
        }
        (accounts, unbound)
    }
    /// The same verified account projection feeds group summaries and presence.
    pub fn channel_member_account_sets(
        &self,
        channel: ConversationId,
    ) -> Result<
        (
            std::collections::BTreeSet<String>,
            std::collections::BTreeSet<String>,
        ),
        NodeError,
    > {
        let members = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            if let Some(state) = book.state(&channel)
                && state.owner_account_id().is_some()
            {
                let accounts = state
                    .members()
                    .iter()
                    .filter(|member| state.is_accepted_member(&member.user_id()))
                    .filter_map(|member| {
                        state
                            .member_account_id(&member.user_id())
                            .map(str::to_owned)
                    })
                    .collect();
                return Ok((accounts, std::collections::BTreeSet::new()));
            }
            book.state(&channel).map(|state| {
                state
                    .members()
                    .iter()
                    .filter(|member| state.is_accepted_member(&member.user_id()))
                    .cloned()
                    .collect::<Vec<_>>()
            })
        };
        let members = match members {
            Some(members) => members,
            None => self
                .account_group_projection(channel)?
                .state
                .map(|state| {
                    state
                        .members()
                        .iter()
                        .filter(|member| state.is_accepted_member(&member.user_id()))
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        };
        Ok(self.project_channel_member_accounts(&members))
    }

    /// Read validated controls independently of delayed plaintext persistence.
    fn channel_control_state(&self, channel: ConversationId) -> Result<ChannelState, NodeError> {
        let state = self
            .account_group_projection(channel)?
            .state
            .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
        if state.is_dissolved() {
            return Err(NodeError::Channel("channel is dissolved".into()));
        }
        Ok(state)
    }

    /// Membership decisions use verified identity, not routing visibility grants.
    fn channel_member_account_id(&self, member: &PublicIdentity) -> Option<String> {
        if *member == self.identity.public() {
            return Some(self.account_id());
        }
        if let Some(account) = self
            .privacy
            .state
            .read()
            .expect("privacy lock not poisoned")
            .as_ref()
            .and_then(|state| state.proofs.account_for(member))
        {
            return Some(account);
        }
        self.roster
            .lock()
            .expect("roster mutex not poisoned")
            .historical_announcements()
            .into_iter()
            .find(|proof| proof.public() == *member && proof.valid_presence())
            .and_then(|proof| proof.account_id())
    }

    /// Bounded invalidations of accepted group information. After lag, consumers
    /// must reload their group inventory; messages do not emit metadata changes.
    pub fn subscribe_channel_changes(&self) -> tokio::sync::broadcast::Receiver<ConversationId> {
        self.channel_changes.subscribe()
    }

    pub(in crate::node) fn schedule_channel_recovery(&self, channel: ConversationId) {
        let mut pending = self
            .channel_recovery_pending
            .lock()
            .expect("recovery queue not poisoned");
        // Overflow is repaired by the rotating anti-entropy fallback.
        if pending.len() < 4096 {
            pending.insert(channel);
        }
        self.channel_recovery_notify.notify_one();
    }

    pub fn channel_recovery(&self, channel: ConversationId) -> Option<ChannelRecovery> {
        let me = self.user_id();
        let book = self.channels.lock().expect("channels mutex not poisoned");
        let state = book.state(&channel)?;
        if !state.is_accepted_member(&me) || state.is_dissolved() || state.members().len() <= 1 {
            return None;
        }
        self.channel_recovery
            .lock()
            .expect("channel recovery lock not poisoned")
            .get(&channel)
            .cloned()
    }

    fn record_channel_recovery(&self, channel: ConversationId, phase: &'static str, checked: bool) {
        let mut observations = self
            .channel_recovery
            .lock()
            .expect("channel recovery lock not poisoned");
        let previous = observations.get(&channel).and_then(|item| item.checked_at);
        if observations.len() >= 4096 && !observations.contains_key(&channel) {
            let oldest = observations
                .iter()
                .min_by_key(|(id, item)| (item.checked_at.unwrap_or(0), *id.as_bytes()))
                .map(|(id, _)| *id);
            if let Some(oldest) = oldest {
                observations.remove(&oldest);
            }
        }
        observations.insert(
            channel,
            ChannelRecovery {
                phase,
                checked_at: if checked {
                    Some(now_millis())
                } else {
                    previous
                },
            },
        );
    }

    /// Pick one verified account sibling for an owner-signed channel invitation.
    /// Discovery and the durable route cache only expose account ids from verified
    /// device certificates; never infer account ownership from display names.
    pub(in crate::node) fn auto_enrollment_candidate(
        &self,
    ) -> Result<Option<(ConversationId, PublicIdentity)>, NodeError> {
        let me = self.user_id();
        let own_account = self.account_id();
        let mut peers = self.routing_peers();
        peers.retain(|peer| !peer.post_office && peer.account_id.is_some());
        peers.sort_unstable_by_key(|peer| peer.public.user_id());
        let by_device: HashMap<_, _> = peers
            .iter()
            .map(|peer| (peer.public.user_id(), peer))
            .collect();

        let book = self.channels.lock().expect("channels mutex not poisoned");
        let mut channel_ids = book.channel_ids();
        channel_ids.sort_unstable_by_key(|id| *id.as_bytes());
        for channel in channel_ids {
            let Some(state) = book.state(&channel) else {
                continue;
            };
            if state.owner() != me || state.is_dissolved() || state.owner_account_id().is_some() {
                continue;
            }
            let projection = self.account_group_projection(channel)?;
            let Some(root) = projection
                .accepted
                .iter()
                .find(|member| member.public == self.identity.public())
                .map(|member| member.membership_event)
            else {
                continue;
            };
            if !self
                .channel_migration
                .lock()
                .expect("channel migration lock")
                .permits(channel, root)
            {
                continue;
            }
            let accepted_accounts: HashSet<&str> = state
                .members()
                .iter()
                .filter_map(|member| {
                    let id = member.user_id();
                    if !state.is_accepted_member(&id) {
                        return None;
                    }
                    if id == me {
                        Some(own_account.as_str())
                    } else {
                        by_device
                            .get(&id)
                            .filter(|peer| peer.public == *member)
                            .and_then(|peer| peer.account_id.as_deref())
                    }
                })
                .collect();
            for peer in &peers {
                if peer
                    .account_id
                    .as_deref()
                    .is_some_and(|account| accepted_accounts.contains(account))
                    && !state.is_member(&peer.public.user_id())
                {
                    return Ok(Some((channel, peer.public.clone())));
                }
            }
        }
        Ok(None)
    }

    /// Persist an owner-signed invitation for one sibling per pass. The ordinary
    /// channel distribution and recovery paths deliver it to the new device.
    pub(in crate::node) async fn auto_enroll_one_channel_sibling(&self) -> Result<bool, NodeError> {
        let Some((channel, device)) = self.auto_enrollment_candidate()? else {
            return Ok(false);
        };
        self.add_channel_member_checked_if(channel, device, true, |commit| commit())
            .await
    }

    /// An invitation may be accepted automatically only when an already accepted
    /// member is a certified sibling of this device's account. Ordinary invitations
    /// continue to require the user's explicit consent.
    pub(in crate::node) fn linked_invite_candidate(&self) -> Option<ConversationId> {
        let me = self.user_id();
        let account = self.account_id();
        let peers: HashMap<_, _> = self
            .routing_peers()
            .into_iter()
            .map(|peer| (peer.public.user_id(), peer))
            .collect();
        let book = self.channels.lock().expect("channels mutex not poisoned");
        let mut channels = book.channel_ids();
        channels.sort_unstable_by_key(|id| *id.as_bytes());
        channels.into_iter().find(|channel| {
            let Some(state) = book.state(channel) else {
                return false;
            };
            state.pending_invite()
                && !state.is_dissolved()
                && state.is_member(&me)
                && state.members().iter().any(|member| {
                    member.user_id() != me
                        && state.is_accepted_member(&member.user_id())
                        && peers.get(&member.user_id()).is_some_and(|peer| {
                            peer.public == *member
                                && !peer.post_office
                                && peer.account_id.as_deref() == Some(account.as_str())
                        })
                })
        })
    }

    pub(in crate::node) async fn auto_accept_one_linked_invite(&self) -> Result<bool, NodeError> {
        let Some(channel) = self.linked_invite_candidate() else {
            return Ok(false);
        };
        self.accept_channel_invite(channel).await?;
        Ok(true)
    }

    /// Run bounded anti-entropy for every host that serves TCP, including CLI and SDK nodes.
    pub(in crate::node) async fn run_channel_recovery_loop(self: Arc<Self>, changes: Arc<Notify>) {
        let mut tick = tokio::time::interval(CHANNEL_RECOVERY_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut cursor = 0usize;
        let mut custody_cursor = 0usize;
        let mut account_maintenance =
            super::channel_account_maintenance::ChannelAccountMaintenance::default();
        let mut retry_after = HashMap::new();
        let mut retry_cycle = HashMap::new();
        let mut recheck_after = HashMap::new();
        let mut recheck_cycle = HashMap::new();
        let node = self.as_ref();
        let mut metadata = self.subscribe_channel_changes();
        let mut network = self.network_changes.subscribe();
        let mut backoff = ChannelRecoveryBackoff::default();
        let mut fallback_at = tokio::time::Instant::now();
        loop {
            let mut discovered = false;
            let mut custody_tick = false;
            tokio::select! {
                _ = changes.notified() => { discovered = true; },
                _ = network.changed() => { discovered = true; },
                _ = self.channel_recovery_notify.notified() => {},
                change = metadata.recv() => match change {
                    Ok(channel) => self.schedule_channel_recovery(channel),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => discovered = true,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                _ = tick.tick() => { custody_tick = true; },
                _ = tokio::time::sleep_until(fallback_at) => {},
            }
            loop {
                match metadata.try_recv() {
                    Ok(channel) => self.schedule_channel_recovery(channel),
                    Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                        discovered = true
                    }
                    Err(_) => break,
                }
            }
            self.maintain_account_channels(&mut account_maintenance)
                .await;
            self.retry_channel_plaintext();
            match self.retry_group_metadata() {
                Ok(channels) => {
                    for channel in channels {
                        let members = self.channel_members(channel);
                        self.distribute_channel(channel, &members).await;
                    }
                }
                Err(error) => log::warn!("channel metadata recovery deferred: {error}"),
            }
            if let Err(error) = self.auto_enroll_one_channel_sibling().await {
                log::warn!("linked channel invitation failed; will retry: {error}");
            }
            if let Err(error) = self.auto_accept_one_linked_invite().await {
                log::warn!("linked channel acceptance failed; will retry: {error}");
            }
            let targets = {
                let mut pending = self
                    .channel_recovery_pending
                    .lock()
                    .expect("recovery queue not poisoned");
                let mut targets: Vec<_> = pending.iter().copied().collect();
                targets.sort_unstable_by_key(|id| *id.as_bytes());
                targets.truncate(CHANNEL_RECOVERY_WAKE_BATCH);
                for channel in &targets {
                    pending.remove(channel);
                }
                if !pending.is_empty() {
                    self.channel_recovery_notify.notify_one();
                }
                targets
            };
            let active = discovered || !targets.is_empty();
            if active {
                backoff.reset();
                fallback_at =
                    fallback_at.min(tokio::time::Instant::now() + CHANNEL_RECOVERY_INTERVAL);
            }
            stream::iter(targets)
                .for_each_concurrent(CHANNEL_RECOVERY_GROUP_CONCURRENCY, |channel| async move {
                    node.pull_channel_from_members(channel, cursor, false).await;
                })
                .await;
            if discovered || tokio::time::Instant::now() >= fallback_at {
                let batch = self.joined_channel_count().min(CHANNEL_RECOVERY_WAKE_BATCH);
                let start = cursor;
                cursor = cursor.wrapping_add(batch);
                stream::iter(start..start.saturating_add(batch))
                    .for_each_concurrent(CHANNEL_RECOVERY_GROUP_CONCURRENCY, |index| async move {
                        node.pull_one_channel_from_member(index).await;
                    })
                    .await;
                // Independent bounded jitter prevents synchronized idle repair bursts.
                let jitter =
                    (now_millis() ^ u64::from(self.identity.public().ed25519_pub[0])) as u8;
                fallback_at = tokio::time::Instant::now() + backoff.next_delay(jitter);
            }
            // Custody and failed plaintext retain their existing retry clock.
            if custody_tick {
                if let Some((channel, event, peer)) = self
                    .retry_one_channel_custody(custody_cursor, &mut retry_after, &mut retry_cycle)
                    .await
                {
                    // No replication exchange used this turn. Repair only the
                    // missing report with the existing exact durable-have query.
                    self.recheck_channel_custody_peer(channel, event, &peer)
                        .await;
                }
                if custody_cursor.is_multiple_of(CHANNEL_CUSTODY_RECHECK_TICKS) {
                    self.recheck_one_channel_custody(
                        custody_cursor / CHANNEL_CUSTODY_RECHECK_TICKS,
                        &mut recheck_after,
                        &mut recheck_cycle,
                    )
                    .await;
                }
                custody_cursor = custody_cursor.wrapping_add(1);
            }
        }
    }

    pub(in crate::node) fn joined_channel_count(&self) -> usize {
        let me = self.user_id();
        let book = self.channels.lock().expect("channels mutex not poisoned");
        book.channel_ids()
            .into_iter()
            .filter(|id| book.state(id).is_some_and(|state| state.is_member(&me)))
            .count()
    }

    /// Publish this device's signed per-channel replica preference. Receiving
    /// messages continues normally; when disabled, inbound peers cannot fetch
    /// this device's channel log or file chunks as a recovery source.
    pub async fn set_channel_replica_enabled(
        &self,
        channel: ConversationId,
        enabled: bool,
    ) -> Result<(), NodeError> {
        self.set_channel_replica_enabled_if(channel, enabled, |commit| commit())
            .await
    }

    pub async fn set_channel_replica_enabled_if(
        &self,
        channel: ConversationId,
        enabled: bool,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        let mut distribution = None;
        authorize(&mut || {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            let members = {
                let book = self.channels.lock().expect("channels mutex not poisoned");
                let state = book
                    .state(&channel)
                    .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
                if !self.channel_identity_is_current(state)? || state.is_dissolved() {
                    return Err(NodeError::Channel("not an active channel member".into()));
                }
                state.members().to_vec()
            };
            self.append_event(channel, EventKind::ChannelReplica, vec![u8::from(enabled)])?;
            self.process_channel(channel);
            distribution = Some(members.clone());
            Ok(())
        })?;
        let Some(members) = distribution else {
            return Ok(());
        };
        self.distribute_channel(channel, &members).await;
        Ok(())
    }

    /// Close a channel for every member while retaining each device's local history.
    pub async fn dissolve_channel(&self, channel: ConversationId) -> Result<(), NodeError> {
        self.dissolve_channel_if(channel, |commit| commit()).await
    }

    pub async fn dissolve_channel_if(
        &self,
        channel: ConversationId,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        let mut distribution = None;
        authorize(&mut || {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            let members = {
                let book = self.channels.lock().expect("channels mutex not poisoned");
                let state = book
                    .state(&channel)
                    .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
                if !self.channel_identity_can_manage(state)? {
                    return Err(NodeError::Channel(
                        "only the channel owner can dissolve it".into(),
                    ));
                }
                if state.is_dissolved() {
                    return Err(NodeError::Channel("channel is already dissolved".into()));
                }
                state.members().to_vec()
            };
            self.append_event(channel, EventKind::ChannelDissolve, Vec::new())?;
            self.process_channel(channel);
            distribution = Some(members.clone());
            Ok(())
        })?;
        let Some(members) = distribution else {
            return Ok(());
        };
        self.distribute_channel(channel, &members).await;
        Ok(())
    }

    /// Stable selected-content cache binding. Ordinary content tails preserve
    /// this token; control changes and reordered canonical history replace it.
    pub(in crate::node) fn channel_content_generation(
        &self,
        channel: ConversationId,
    ) -> Result<Option<([u8; 16], i64)>, NodeError> {
        let Some(source) = self.channel_checkpoints.source_binding(channel)? else {
            return Ok(None);
        };
        Ok(Some((
            source.job,
            self.channel_checkpoints.control_stamp(&source)?,
        )))
    }

    /// Exact invitation cutoff for one selected event, including own authors.
    /// Receive-chain admission has a separate stricter owner.
    pub(in crate::node) fn channel_content_visible(
        &self,
        channel: ConversationId,
        event: crate::eventlog::EventId,
    ) -> Result<bool, LogError> {
        self.channel_checkpoints.content_visible(channel, event)
    }

    /// Event ids visible to this device after it accepted an invitation.
    /// `None` means this channel has no acceptance cutoff (or is not a channel).
    #[cfg(test)]
    pub(in crate::node) fn channel_content_gate(
        &self,
        channel: ConversationId,
    ) -> Result<Option<HashSet<EventId>>, LogError> {
        let status = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            book.state(&channel)
                .map(|state| (state.pending_invite(), state.accepted_event_id()))
        };
        let Some(status) = status else {
            return Ok(None);
        };
        if status.0 {
            return Ok(Some(HashSet::new()));
        }
        let Some(accepted) = status.1 else {
            return Ok(None);
        };
        let log = self.log.lock().expect("log mutex not poisoned");
        let mut descendants = HashSet::new();
        for event in log.events(&channel)? {
            if event.id == accepted
                || event
                    .parents
                    .iter()
                    .any(|parent| descendants.contains(parent))
            {
                descendants.insert(event.id);
            }
        }
        Ok(Some(descendants))
    }
    /// Create a channel named `name` with `members` (the creator is added
    /// automatically). Mints a channel id, posts the membership event, builds our own
    /// channel state, then distributes our epoch-0 sender-key distribution to the
    /// members via a `KeyRotation` event so they can follow our chain.
    pub async fn create_channel(
        &self,
        name: &str,
        members: Vec<PublicIdentity>,
    ) -> Result<ConversationId, NodeError> {
        self.create_channel_with_consent(name, members, false, None, |commit| commit())
            .await
    }

    /// Legacy device-only invitations for compatibility consumers. Desktop
    /// creation uses `create_account_channel_invited_if` and account acceptance.
    pub async fn create_channel_invited(
        &self,
        name: &str,
        members: Vec<PublicIdentity>,
    ) -> Result<ConversationId, NodeError> {
        self.create_channel_invited_if(name, members, |commit| commit())
            .await
    }

    pub async fn create_channel_invited_if(
        &self,
        name: &str,
        members: Vec<PublicIdentity>,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<ConversationId, NodeError> {
        self.create_channel_with_consent(name, members, true, None, authorize)
            .await
    }

    /// Create an account-owned group using verified account invitations.
    pub async fn create_account_channel_invited_if(
        &self,
        name: &str,
        member_account_ids: Vec<String>,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<ConversationId, NodeError> {
        use super::channel_account_authority::{AccountAuthorityControl, AccountGrant};
        // Account IDs stay authoritative while the shared invitation owner selects routes.
        // Authenticate bindings and format support before creating any group.
        let member_account_ids: std::collections::BTreeSet<_> =
            member_account_ids.into_iter().collect();
        if member_account_ids.len() > 255 {
            return Err(NodeError::Channel("too many invitation targets".into()));
        }
        // Verification belongs to one caller lifetime, even when selected
        // accounts advertise many stalled devices. Expiry rejects the complete
        // selection before host admission or any durable group creation.
        let mut bindings =
            tokio::time::timeout(super::route_timing::MAX_AUTHENTICATION_DEADLINE, async {
                let mut bindings = std::collections::BTreeMap::new();
                for expected_account in member_account_ids {
                    if expected_account == self.account_id() {
                        continue;
                    }
                    let proof = self
                        .verified_account_invitation_target(&expected_account)
                        .await?;
                    let account = proof
                        .account_cert
                        .as_ref()
                        .expect("verified account certificate")
                        .account_ed25519_pub;
                    bindings.insert(account, proof);
                }
                Ok::<_, NodeError>(bindings)
            })
            .await
            .map_err(|_| {
                NodeError::Channel("account invitation verification timed out".into())
            })??;
        let own = self.own_group_proof();
        bindings.insert(self.account.public().ed25519_pub, own.clone());
        let authority = AccountAuthorityControl {
            version: 1,
            previous: None,
            issuer: own,
            owner_account: self.account.public().ed25519_pub,
            grants: bindings
                .iter()
                .map(|(account, proof)| {
                    let mut incarnation = [0; 16];
                    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut incarnation);
                    AccountGrant {
                        incarnation,
                        pending: *account != self.account.public().ed25519_pub,
                        original: proof.clone(),
                        membership_event: EventId::new([0; 32]),
                        acceptance_event: None,
                    }
                })
                .collect(),
            revoked: Vec::new(),
            legacy_bindings: bindings.into_values().collect(),
            account_signature: vec![0; 64],
        };
        if AccountAuthorityControl::decode(&authority.encode()).is_none() {
            return Err(NodeError::Channel(
                "account invitations exceed group format limits".into(),
            ));
        }
        let members = authority
            .legacy_bindings
            .iter()
            .map(|proof| proof.public())
            .collect();
        self.create_channel_with_consent(name, members, true, Some(authority), authorize)
            .await
    }

    async fn create_channel_with_consent(
        &self,
        name: &str,
        mut members: Vec<PublicIdentity>,
        invite: bool,
        mut authority: Option<super::channel_account_authority::AccountAuthorityControl>,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<ConversationId, NodeError> {
        let mut distribution = None;
        authorize(&mut || {
            let me = self.identity.public();
            let owner = me.user_id();
            if !members.iter().any(|m| m.user_id() == owner) {
                members.push(me);
            }
            let members = canonical_members(std::mem::take(&mut members));
            let channel = crate::channel::new_channel_id();
            let meta = ChannelMeta {
                name: name.to_string(),
                members: members.clone(),
                epoch: 0,
                // The creator is the initial owner. Administration can later transfer
                // through a separately signed control event.
                owner: owner.clone(),
            };

            let membership = if invite {
                meta.encode_invitation()
            } else {
                meta.encode()
            };
            let history_policy = || {
                super::join_history::encode_policy(
                    crate::channel::HistoryPolicy::LastTenWithinThreeDays,
                )
            };
            if let Some(control) = authority.as_mut() {
                // A crash must not publish a device-only genesis without its
                // account grants. Reuse the existing atomic new-channel store.
                let mut delivery = self.delivery.lock().expect("delivery mutex");
                self.recover_delivery(&mut delivery)
                    .map_err(NodeError::Log)?;
                let mut log = self.log.lock().expect("log mutex");
                let wall = now_millis();
                let root = crate::eventlog::Event::new(
                    &self.identity,
                    channel,
                    1,
                    vec![],
                    1,
                    wall,
                    EventKind::MembershipChange,
                    membership,
                );
                for grant in &mut control.grants {
                    grant.membership_event = root.id;
                }
                let account = crate::eventlog::Event::new(
                    &self.identity,
                    channel,
                    2,
                    vec![root.id],
                    2,
                    wall,
                    EventKind::ChannelAccountAuthority,
                    control.signed_encode(channel, &self.account),
                );
                let policy = crate::eventlog::Event::new(
                    &self.identity,
                    channel,
                    3,
                    vec![account.id],
                    3,
                    wall,
                    EventKind::ChannelHistoryPolicy,
                    history_policy(),
                );
                log.append_channel_batch_atomic(&[root, account, policy])
                    .map_err(NodeError::Log)?;
            } else {
                self.append_event(channel, EventKind::MembershipChange, membership)?;
                if invite {
                    self.append_event(channel, EventKind::ChannelHistoryPolicy, history_policy())?;
                }
            }
            distribution = Some((channel, members.clone()));
            Ok(())
        })?;
        let (channel, members) = distribution.expect("authorized channel commit executed");
        self.await_channel_metadata(channel).await?;
        self.record_channel_owner_origin(channel)?;
        self.distribute_my_sender_key(channel, &members)?;
        self.distribute_channel(channel, &members).await;
        Ok(channel)
    }

    /// Generate (if needed) our current-epoch sender-key distribution for `channel`
    /// and post a `KeyRotation` event carrying it sealed to each member. Members open
    /// their entry to follow our chain from `n = 0`. Called at create and on each
    /// membership change so members at a new epoch can read our new messages.
    pub(in crate::node) fn distribute_my_sender_key(
        &self,
        channel: ConversationId,
        members: &[PublicIdentity],
    ) -> Result<(), NodeError> {
        let _ = members;
        let mut delivery = self.delivery.lock().expect("delivery lock");
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        let book = self.channels.lock().expect("channels lock");
        let state = book
            .state(&channel)
            .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
        if !self.channel_identity_is_current(state)? || state.is_dissolved() {
            return Err(NodeError::Channel("not an active channel member".into()));
        }
        let mut senders = state.export_my_senders();
        let key = senders
            .iter()
            .find(|(epoch, _)| *epoch == state.epoch())
            .and_then(|(_, bytes)| crate::channel::sender_key::SenderKey::deserialize(bytes))
            .unwrap_or_else(crate::channel::sender_key::SenderKey::generate);
        senders.retain(|(epoch, _)| *epoch != state.epoch());
        senders.push((state.epoch(), key.serialize()));
        let sealed = seal_keys_for(
            &self.identity,
            &state.recipient_members(),
            &key.distribution(),
            state.epoch(),
        )
        .map_err(|e| NodeError::Channel(e.to_string()))?;
        let log = self.log.lock().expect("log lock");
        let author =
            crate::eventlog::event::Author::from_ed25519(self.identity.public().ed25519_pub);
        let seq = log
            .next_sequence(&channel, &author)
            .map_err(NodeError::Log)?;
        let (parents, lamport) = log.prepare(&channel).map_err(NodeError::Log)?;
        let clock = now_millis();
        let event = crate::eventlog::event::Event::new(
            &self.identity,
            channel,
            seq,
            parents,
            lamport,
            clock,
            EventKind::KeyRotation,
            sealed.encode(),
        );
        log.sync().map_err(NodeError::Log)?;
        drop(log);
        delivery
            .begin(
                super::delivery_store::DeliveryTransaction::ChannelOutgoing {
                    events: vec![event],
                    senders,
                    sent: super::sentlog::SentEntry {
                        conversation: channel,
                        seq,
                        wall_clock: clock,
                        plaintext: vec![],
                    },
                },
            )
            .map_err(NodeError::Log)?;
        drop(book);
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        drop(delivery);
        self.process_channel(channel);
        Ok(())
    }

    /// Add `new_member` to `channel`, bumping the epoch. Posts a `MembershipChange`
    /// carrying the new member set at `old_epoch + 1`, adopts it locally, then pushes
    /// the channel to the new member set (so the joiner gets the membership event). The
    /// fresh-epoch sender key is distributed lazily on our next send (the
    /// `has_my_sender(new_epoch)` guard in the send path), so the joiner can read
    /// post-join messages but not pre-join ones.
    pub async fn add_channel_member(
        &self,
        channel: ConversationId,
        new_member: PublicIdentity,
    ) -> Result<(), NodeError> {
        self.add_channel_member_if(channel, new_member, |commit| commit())
            .await
    }

    pub async fn add_channel_member_if(
        &self,
        channel: ConversationId,
        new_member: PublicIdentity,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        self.add_channel_member_checked_if(channel, new_member, false, authorize)
            .await
            .map(|_| ())
    }

    /// Auto-enrollment must revalidate its accepted-account evidence inside the
    /// same commit boundary as account removal, before signing an invitation.
    pub(in crate::node) async fn add_channel_member_checked_if(
        &self,
        channel: ConversationId,
        new_member: PublicIdentity,
        require_account_sibling: bool,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<bool, NodeError> {
        let upgraded = self
            .account_group_projection(channel)?
            .authority
            .current_event()
            .is_some();
        let verified = if upgraded {
            let peer = self
                .routing_peer(&new_member.user_id())
                .filter(|peer| peer.public == new_member)
                .ok_or_else(|| {
                    NodeError::Channel(
                        "the invited account must be reachable with an updated client".into(),
                    )
                })?;
            let Some((
                super::channel_account_bootstrap::AccountGroupResponse::Capability {
                    version: 1,
                    ..
                },
                proof,
            )) = self
                .query_account_groups_verified(
                    &peer,
                    super::channel_account_bootstrap::AccountGroupRequest::Capability,
                )
                .await
            else {
                return Err(NodeError::Channel(
                    "the invited account needs a compatible client before joining this group"
                        .into(),
                ));
            };
            if peer.account_id != proof.account_id() {
                return Err(NodeError::Channel(
                    "the invited account identity changed; refresh contacts and retry".into(),
                ));
            }
            Some(proof)
        } else {
            None
        };
        // Policy serialization precedes host authorization and membership locks.
        // Never retain it during network distribution.
        let policy = self.privacy.gate.read().await;
        let mut distribution = None;
        authorize(&mut || {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            if self
                .account_group_projection(channel)?
                .authority
                .current_event()
                .is_some()
            {
                let proof = verified.as_ref().ok_or_else(|| {
                    NodeError::Channel(
                        "group authority changed; retry the account invitation".into(),
                    )
                })?;
                match self.try_account_channel_invite(channel, &new_member, proof)? {
                    super::channel_account_controls::AccountChannelMutation::Changed(members) => {
                        distribution = Some(members);
                        return Ok(());
                    }
                    super::channel_account_controls::AccountChannelMutation::Unchanged => {
                        return Ok(());
                    }
                    super::channel_account_controls::AccountChannelMutation::Legacy => {
                        return Err(NodeError::Channel(
                            "group authority changed; retry the account invitation".into(),
                        ));
                    }
                }
            }
            if require_account_sibling
                && self.auto_enrollment_candidate()? != Some((channel, new_member.clone()))
            {
                return Ok(());
            }
            if require_account_sibling {
                let durable = self.channel_control_state(channel)?;
                let book = self.channels.lock().expect("channels mutex not poisoned");
                if book.state(&channel).is_none_or(|state| {
                    state.epoch() != durable.epoch()
                        || state.owner() != durable.owner()
                        || state.members() != durable.members()
                }) {
                    return Ok(());
                }
            }
            let (name, mut new_members, new_epoch, owner) = {
                let state = self.channel_control_state(channel)?;
                (
                    state.name().to_string(),
                    state.members().to_vec(),
                    state.epoch() + 1,
                    state.owner().to_string(),
                )
            };
            // Only the owner may change membership — a non-owner's event would be rejected by
            // every node's `process`, so refuse it locally too (no useless event/epoch bump).
            if !self.channel_can_manage(channel)? {
                return Err(NodeError::Channel(
                    "only the channel owner can change membership".into(),
                ));
            }
            // Already a member → no-op. Don't bump the epoch / force a re-key for nothing.
            if new_members
                .iter()
                .any(|m| m.user_id() == new_member.user_id())
            {
                return Ok(());
            }
            new_members.push(new_member.clone());
            let new_members = canonical_members(new_members);
            let meta = ChannelMeta {
                name,
                members: new_members.clone(),
                epoch: new_epoch,
                owner,
            };
            let policy = self.channel_history_policy(channel)?;
            let selected = self.select_join_history(channel, policy, now_millis())?;
            self.append_invitation_with_history(channel, &meta, &new_member, selected, policy)?;
            self.process_channel(channel);
            distribution = Some(new_members.clone());
            Ok(())
        })?;
        drop(policy);
        let Some(new_members) = distribution else {
            return Ok(false);
        };
        self.distribute_channel(channel, &new_members).await;
        Ok(true)
    }

    /// Remove the member with `member_user_id` from `channel`, bumping the epoch. Posts
    /// a `MembershipChange` carrying the reduced member set at `old_epoch + 1`, adopts it
    /// locally, then pushes the channel to the remaining members only — the removed
    /// member never receives the new epoch's events or sender keys, so it cannot read
    /// post-removal messages.
    pub async fn remove_channel_member(
        &self,
        channel: ConversationId,
        member_user_id: &str,
    ) -> Result<(), NodeError> {
        self.remove_channel_member_if(channel, member_user_id, |commit| commit())
            .await
    }

    pub async fn remove_channel_member_if(
        &self,
        channel: ConversationId,
        member_user_id: &str,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        // Policy serialization precedes host authorization and membership locks.
        // Never retain it during network distribution.
        let policy = self.privacy.gate.read().await;
        let mut distribution = None;
        authorize(&mut || {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            let projection = self.account_group_projection(channel)?;
            if projection.authority.current_event().is_some() {
                let account = projection
                    .state
                    .as_ref()
                    .and_then(|state| state.member_account_id(member_user_id))
                    .unwrap_or("");
                match self.try_account_channel_remove(channel, account)? {
                    super::channel_account_controls::AccountChannelMutation::Changed(members) => {
                        distribution = Some(members);
                        return Ok(());
                    }
                    super::channel_account_controls::AccountChannelMutation::Unchanged => {
                        return Ok(());
                    }
                    super::channel_account_controls::AccountChannelMutation::Legacy => {
                        return Err(NodeError::Channel(
                            "group authority changed; retry account removal".into(),
                        ));
                    }
                }
            }
            let (name, mut new_members, new_epoch, owner) = {
                let state = self.channel_control_state(channel)?;
                (
                    state.name().to_string(),
                    state.members().to_vec(),
                    state.epoch() + 1,
                    state.owner().to_string(),
                )
            };
            // Only the owner may change membership — reject a non-owner's removal locally
            // (every node would reject the resulting event anyway).
            if !self.channel_can_manage(channel)? {
                return Err(NodeError::Channel(
                    "only the channel owner can change membership".into(),
                ));
            }
            if member_user_id == owner {
                return Err(NodeError::Channel(
                    "transfer channel ownership before the owner leaves".into(),
                ));
            }
            // Not a member → no-op (don't bump the epoch / re-key for nothing).
            if !new_members.iter().any(|m| m.user_id() == member_user_id) {
                return Ok(());
            }
            // Refuse to remove the last member — it would leave an unreadable, keyless channel.
            if new_members.len() <= 1 {
                return Err(NodeError::Channel(
                    "cannot remove the last member of a channel".into(),
                ));
            }
            new_members.retain(|m| m.user_id() != member_user_id);
            let new_members = canonical_members(new_members);
            let meta = ChannelMeta {
                name,
                members: new_members.clone(),
                epoch: new_epoch,
                owner,
            };
            self.append_event(channel, EventKind::MembershipChange, meta.encode())?;
            self.process_channel(channel);
            distribution = Some(new_members.clone());
            Ok(())
        })?;
        drop(policy);
        let Some(new_members) = distribution else {
            return Ok(());
        };
        self.distribute_channel(channel, &new_members).await;
        Ok(())
    }

    /// Remove a person and all of its currently certified channel devices in
    /// one owner-signed epoch. Never infer an account from a display name.
    pub async fn remove_channel_account_members_if(
        &self,
        channel: ConversationId,
        account_id: &str,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        // Policy serialization precedes host authorization and membership locks.
        // Never retain it during network distribution.
        let policy = self.privacy.gate.read().await;
        let mut distribution = None;
        authorize(&mut || {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            match self.try_account_channel_remove(channel, account_id)? {
                super::channel_account_controls::AccountChannelMutation::Changed(members) => {
                    distribution = Some(members);
                    return Ok(());
                }
                super::channel_account_controls::AccountChannelMutation::Unchanged => return Ok(()),
                super::channel_account_controls::AccountChannelMutation::Legacy => {}
            }
            let (name, members, epoch, owner) = {
                let state = self.channel_control_state(channel)?;
                (
                    state.name().to_owned(),
                    state.members().to_vec(),
                    state.epoch(),
                    state.owner().to_owned(),
                )
            };
            if !self.channel_can_manage(channel)? {
                return Err(NodeError::Channel(
                    "only the channel owner can change membership".into(),
                ));
            }
            let own_account = self.account_id();
            let belongs = |member: &PublicIdentity| {
                if member.user_id() == self.user_id() {
                    own_account == account_id
                } else {
                    self.channel_member_account_id(member).as_deref() == Some(account_id)
                }
            };
            if members
                .iter()
                .any(|member| member.user_id() == owner && belongs(member))
            {
                return Err(NodeError::Channel(
                    "transfer channel ownership before the owner leaves".into(),
                ));
            }
            let remaining = canonical_members(
                members
                    .iter()
                    .filter(|member| !belongs(member))
                    .cloned()
                    .collect(),
            );
            if remaining.len() == members.len() {
                return Ok(());
            }
            if remaining.is_empty() {
                return Err(NodeError::Channel(
                    "cannot remove the last member of a channel".into(),
                ));
            }
            let meta = ChannelMeta {
                name,
                members: remaining.clone(),
                epoch: epoch
                    .checked_add(1)
                    .ok_or_else(|| NodeError::Channel("channel epoch exhausted".into()))?,
                owner,
            };
            self.append_event(channel, EventKind::MembershipChange, meta.encode())?;
            self.process_channel(channel);
            distribution = Some(remaining);
            Ok(())
        })?;
        drop(policy);
        if let Some(members) = distribution {
            self.distribute_channel(channel, &members).await;
        }
        Ok(())
    }

    /// Rename `channel` to `new_name`. Posts a `ChannelRename` event carrying just the new
    /// name; it's applied last-writer-wins by `(lamport, id)`, INDEPENDENT of the membership
    /// epoch — so a rename neither rotates sender keys nor races a concurrent add/remove (a
    /// shared-epoch `MembershipChange` could clobber, or be clobbered by, a rename). Owner-
    /// only: a non-owner's event is dropped by every node's `process`, so it's refused here
    /// too. The new name converges on every member via the usual distribution.
    pub async fn rename_channel(
        &self,
        channel: ConversationId,
        new_name: &str,
    ) -> Result<(), NodeError> {
        self.rename_channel_if(channel, new_name, |commit| commit())
            .await
    }

    pub async fn rename_channel_if(
        &self,
        channel: ConversationId,
        new_name: &str,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        let mut distribution = None;
        authorize(&mut || {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            let (current_name, members) = {
                let book = self.channels.lock().expect("channels mutex not poisoned");
                let state = book
                    .state(&channel)
                    .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
                (state.name().to_string(), state.members().to_vec())
            };
            // Only the owner may rename — a non-owner's event would be rejected by every node's
            // `process`, so refuse it locally too.
            if !self.channel_can_manage(channel)? {
                return Err(NodeError::Channel(
                    "only the channel owner can rename the channel".into(),
                ));
            }
            let new_name = normalize_simple_name(new_name)?;
            // Unchanged name → no-op (don't post a redundant event).
            if new_name == current_name {
                return Ok(());
            }
            self.append_event(
                channel,
                EventKind::ChannelRename,
                new_name.as_bytes().to_vec(),
            )?;
            self.process_channel(channel);
            distribution = Some(members.clone());
            Ok(())
        })?;
        let Some(members) = distribution else {
            return Ok(());
        };
        self.distribute_channel(channel, &members).await;
        Ok(())
    }

    /// Hand channel administration to an existing member. The owner-signed event
    /// changes authority without changing membership or rotating message keys.
    pub async fn transfer_channel_owner(
        &self,
        channel: ConversationId,
        new_owner: &str,
    ) -> Result<(), NodeError> {
        self.transfer_channel_owner_if(channel, new_owner, |commit| commit())
            .await
    }

    pub async fn transfer_channel_owner_if(
        &self,
        channel: ConversationId,
        new_owner: &str,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        let mut distribution = None;
        authorize(&mut || {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            match self.try_account_channel_transfer(channel, new_owner)? {
                super::channel_account_controls::AccountChannelMutation::Changed(members) => {
                    distribution = Some(members);
                    return Ok(());
                }
                super::channel_account_controls::AccountChannelMutation::Unchanged => return Ok(()),
                super::channel_account_controls::AccountChannelMutation::Legacy => {}
            }
            let (owner, accepted) = {
                let book = self.channels.lock().expect("channels mutex not poisoned");
                let state = book
                    .state(&channel)
                    .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
                (
                    state.owner().to_string(),
                    state.is_accepted_member(new_owner),
                )
            };
            if !self.channel_can_manage(channel)? {
                return Err(NodeError::Channel(
                    "only the channel owner can transfer ownership".into(),
                ));
            }
            if new_owner == owner {
                return Ok(());
            }
            if !accepted {
                return Err(NodeError::Channel(
                    "new owner must have accepted the channel invitation".into(),
                ));
            }
            self.append_event(
                channel,
                EventKind::ChannelOwnerTransfer,
                new_owner.as_bytes().to_vec(),
            )?;
            self.process_channel(channel);
            let members = {
                let book = self.channels.lock().expect("channels mutex not poisoned");
                let state = book
                    .state(&channel)
                    .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
                if state.owner() != new_owner {
                    return Err(NodeError::Channel(
                        "channel membership changed before ownership transfer completed".into(),
                    ));
                }
                state.members().to_vec()
            };
            distribution = Some(members.clone());
            Ok(())
        })?;
        let Some(members) = distribution else {
            return Ok(());
        };
        self.distribute_channel(channel, &members).await;
        Ok(())
    }

    /// Publish a self-signed departure, then stop participating locally. The event is
    /// first offered to the old member set while our old group route is still active.
    pub async fn leave_channel(&self, channel: ConversationId) -> Result<(), NodeError> {
        self.leave_channel_if(channel, |commit| commit()).await
    }

    pub async fn leave_channel_if(
        &self,
        channel: ConversationId,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        let own_proof = self.own_group_proof();
        let mut distribution = None;
        authorize(&mut || {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            if self
                .account_group_projection(channel)?
                .authority
                .current_event()
                .is_some()
            {
                distribution = Some(self.apply_account_self_control(
                    channel,
                    account_self_control::AccountSelfControlInput::Leave,
                    &own_proof,
                )?);
                return Ok(());
            }
            distribution = Some(self.apply_legacy_member_leave(channel, &own_proof)?);
            Ok(())
        })?;
        let Some(members) = distribution else {
            return Ok(());
        };
        self.distribute_channel(channel, &members).await;
        self.process_channel(channel);
        Ok(())
    }

    /// Accept a pending invitation. The signed event makes the decision survive
    /// restart and sync to other copies of this device's log.
    pub async fn accept_channel_invite(&self, channel: ConversationId) -> Result<(), NodeError> {
        self.accept_channel_invite_if(channel, |commit| commit())
            .await
    }

    pub async fn accept_channel_invite_if(
        &self,
        channel: ConversationId,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        let own_proof = self.own_group_proof();
        let mut distribution = None;
        authorize(&mut || {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            if self
                .account_group_projection(channel)?
                .authority
                .current_event()
                .is_some()
            {
                distribution = Some(self.apply_account_self_control(
                    channel,
                    account_self_control::AccountSelfControlInput::Accept,
                    &own_proof,
                )?);
                return Ok(());
            }
            let members = {
                let book = self.channels.lock().expect("channels mutex not poisoned");
                let state = book
                    .state(&channel)
                    .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
                if !state.pending_invite() || !self.channel_identity_matches_account(state) {
                    return Err(NodeError::Channel("no pending invitation".into()));
                }
                state.members().to_vec()
            };
            self.append_event(channel, EventKind::ChannelAccept, vec![])?;
            self.process_channel(channel);
            distribution = Some(members.clone());
            Ok(())
        })?;
        let Some(members) = distribution else {
            return Ok(());
        };
        self.distribute_channel(channel, &members).await;
        Ok(())
    }

    /// Retry signed departures after a peer was offline during the initial push.
    /// The channel book keeps only departures that were valid when replayed, so a
    /// forged leave event cannot open an outbound route to an unrelated peer.
    pub async fn retry_channel_departures(&self) {
        self.retry_channel_controls().await;
    }

    pub async fn retry_channel_controls(&self) {
        let mut released = self.retry_join_history().unwrap_or_else(|error| {
            log::warn!("channel join history retry failed: {error}");
            Vec::new()
        });
        released.extend(self.retry_group_metadata().unwrap_or_else(|error| {
            log::warn!("group metadata retry failed: {error}");
            Vec::new()
        }));
        let mut pending = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let own = self.user_id();
            let mut controls = std::collections::HashMap::new();
            for (id, members) in book
                .pending_departures(&own)
                .into_iter()
                .chain(book.pending_acceptances(&own))
                .chain(book.pending_transfers(&own))
                .chain(book.pending_replica_preferences(&own))
            {
                controls.insert(id, members);
            }
            for channel in released {
                if let Some(state) = book.state(&channel) {
                    controls.insert(channel, state.members().to_vec());
                }
            }
            controls.into_iter().collect::<Vec<_>>()
        };
        pending.sort_by_key(|(id, _)| *id.as_bytes());
        let len = pending.len();
        if len == 0 {
            return;
        }
        let offset = (now_millis() / 10_000) as usize % len;
        for (channel, members) in pending.into_iter().cycle().skip(offset).take(len.min(32)) {
            self.distribute_channel(channel, &members).await;
        }
    }

    /// Send a message to a channel we hold the key for. Distribution is best-effort
    /// and fail-soft: returns `Ok(())` once the event is appended locally, regardless
    /// of how many members were reachable (an offline member catches up via sync).
    pub async fn send_channel_message(
        &self,
        channel: ConversationId,
        text: &[u8],
    ) -> Result<(), NodeError> {
        self.send_channel_message_reply(channel, text, None).await
    }

    /// Send a channel message that replies to `reply_to` (or `None` for a normal message).
    pub async fn send_channel_message_reply(
        &self,
        channel: ConversationId,
        text: &[u8],
        reply_to: Option<EventId>,
    ) -> Result<(), NodeError> {
        self.send_channel_message_reply_expiring(channel, text, reply_to, None)
            .await
    }

    pub async fn send_channel_message_reply_expiring(
        &self,
        channel: ConversationId,
        text: &[u8],
        reply_to: Option<EventId>,
        expires_at_ms: Option<u64>,
    ) -> Result<(), NodeError> {
        self.send_channel_message_reply_expiring_if(
            channel,
            text,
            reply_to,
            expires_at_ms,
            |commit| commit(),
        )
        .await
    }

    pub async fn send_channel_message_reply_expiring_if(
        &self,
        channel: ConversationId,
        text: &[u8],
        reply_to: Option<EventId>,
        expires_at_ms: Option<u64>,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        let id = self.enqueue_channel_message_reply_expiring_if(
            channel,
            text,
            reply_to,
            expires_at_ms,
            &mut authorize,
        )?;
        self.flush_accepted_message(&MessageDestination::Channel(channel), id)
            .await;
        Ok(())
    }

    /// Send an animated sticker to a channel: `sticker_id` is the bundled sticker's
    /// codepoint id, `fallback` the emoji char shown if a member lacks it.
    pub async fn send_sticker_channel(
        &self,
        channel: ConversationId,
        sticker_id: &str,
        fallback: &[u8],
    ) -> Result<(), NodeError> {
        self.send_sticker_channel_expiring(channel, sticker_id, fallback, None)
            .await
    }

    pub async fn send_sticker_channel_expiring(
        &self,
        channel: ConversationId,
        sticker_id: &str,
        fallback: &[u8],
        expires_at_ms: Option<u64>,
    ) -> Result<(), NodeError> {
        self.send_sticker_channel_expiring_if(
            channel,
            sticker_id,
            fallback,
            expires_at_ms,
            |commit| commit(),
        )
        .await
    }

    pub async fn send_sticker_channel_expiring_if(
        &self,
        channel: ConversationId,
        sticker_id: &str,
        fallback: &[u8],
        expires_at_ms: Option<u64>,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        let id = self.enqueue_sticker_channel_expiring_if(
            channel,
            sticker_id,
            fallback,
            expires_at_ms,
            &mut authorize,
        )?;
        self.flush_accepted_message(&MessageDestination::Channel(channel), id)
            .await;
        Ok(())
    }

    /// Commit a channel message locally without waiting for network fanout.
    pub fn enqueue_channel_message_reply_expiring_if(
        &self,
        channel: ConversationId,
        text: &[u8],
        reply_to: Option<EventId>,
        expires_at_ms: Option<u64>,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<EventId, NodeError> {
        super::expiry_policy::validate_outgoing(expires_at_ms)?;
        self.enqueue_channel_body_if(
            channel,
            crate::expiry::wrap(
                &MessageBody::new(text.to_vec(), reply_to).encode(),
                expires_at_ms,
            ),
            None,
            &mut authorize,
        )
    }

    pub fn enqueue_protected_channel_message_reply_expiring_if(
        &self,
        channel: ConversationId,
        text: &[u8],
        reply_to: Option<EventId>,
        expires_at_ms: Option<u64>,
        protection: super::MessageProtection,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<EventId, NodeError> {
        super::expiry_policy::validate_outgoing(expires_at_ms)?;
        self.enqueue_channel_body_if(
            channel,
            crate::expiry::wrap(
                &MessageBody::protected(text.to_vec(), reply_to, protection).encode(),
                expires_at_ms,
            ),
            None,
            &mut authorize,
        )
    }

    /// Commit a channel sticker locally; custody progresses independently.
    pub fn enqueue_sticker_channel_expiring_if(
        &self,
        channel: ConversationId,
        sticker_id: &str,
        fallback: &[u8],
        expires_at_ms: Option<u64>,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<EventId, NodeError> {
        super::expiry_policy::validate_outgoing(expires_at_ms)?;
        self.enqueue_channel_body_if(
            channel,
            crate::expiry::wrap(
                &MessageBody::sticker(sticker_id.to_owned(), fallback.to_vec()).encode(),
                expires_at_ms,
            ),
            None,
            &mut authorize,
        )
    }

    /// Push a control event to every known member, including on large channels.
    /// Membership changes and key rotations must not depend on gossip convergence.
    pub(in crate::node) async fn distribute_channel(
        &self,
        channel: ConversationId,
        members: &[PublicIdentity],
    ) {
        if let Err(error) = self.await_channel_metadata(channel).await {
            log::warn!("accepted channel control preparation deferred: {error}");
            return;
        }
        self.distribute_channel_targets(channel, members, None, true, None)
            .await;
    }

    /// Full control distribution with one exact-message custody probe on each
    /// existing sync exchange. Account operations retain all-member fanout.
    pub(in crate::node) async fn distribute_channel_confirming(
        &self,
        channel: ConversationId,
        members: &[PublicIdentity],
        event: Option<EventId>,
    ) -> bool {
        if let Err(error) = self.await_channel_metadata(channel).await {
            log::warn!("accepted channel control preparation deferred: {error}");
            return false;
        }
        self.distribute_channel_targets(channel, members, None, true, event)
            .await
    }

    /// Finish direct offer/chunk fanout before optional relay custody. Waiting
    /// for a relay after the offer but before the chunks strands live receivers
    /// at "receiving" despite an available direct application route.
    pub(in crate::node) async fn distribute_channel_file(
        &self,
        channel: ConversationId,
        file: ConversationId,
        members: &[PublicIdentity],
        automatic_inline: bool,
    ) {
        self.distribute_channel_targets(channel, members, None, false, None)
            .await;
        if automatic_inline {
            self.distribute_channel_targets(file, members, None, false, None)
                .await;
        }
        let recipients: Vec<_> = members
            .iter()
            .filter(|member| member.user_id() != self.user_id())
            .cloned()
            .collect();
        let _ = self
            .replicate_to_post_office_targets(channel, recipients.clone())
            .await;
        if automatic_inline {
            let _ = self
                .replicate_to_post_office_targets(file, recipients)
                .await;
        }
    }

    /// For a large channel, seed a few replicas and let member gossip plus
    /// anti-entropy complete the distribution. Small channels retain direct fanout.
    pub(in crate::node) async fn distribute_channel_message(
        &self,
        channel: ConversationId,
        members: &[PublicIdentity],
        event_id: EventId,
    ) {
        let started = Instant::now();
        let me = self.user_id();
        let peers: Vec<_> = members
            .iter()
            .filter(|member| member.user_id() != me)
            .filter_map(|member| {
                self.routing_peer(&member.user_id())
                    .filter(|peer| peer.public == *member)
            })
            .collect();
        let targets = select_channel_targets(
            peers,
            members.len(),
            usize::from(event_id.as_bytes()[0]),
            LARGE_CHANNEL_INITIAL_FANOUT,
        );
        let seeded = targets.len();
        // There are at most four targets (a small channel's other members) or
        // two (a large channel's seed replicas). Start them together so a
        // stalled handshake cannot block delivery to another reachable member.
        let sent_frame_bytes = join_all(targets.iter().map(|peer| async {
            tokio::time::timeout(
                CHANNEL_RECOVERY_PEER_TIMEOUT,
                self.sync_and_confirm_custody(channel, event_id, peer),
            )
            .await
            .unwrap_or(0)
        }))
        .await
        .into_iter()
        .fold(0u64, u64::saturating_add);
        let _ = self
            .replicate_to_post_office_targets_confirming(
                channel,
                members
                    .iter()
                    .filter(|member| member.user_id() != me)
                    .cloned()
                    .collect(),
                Some(event_id),
            )
            .await;
        #[cfg(test)]
        {
            *self
                .channel_last_send
                .lock()
                .expect("channel send mutex not poisoned") = Some((seeded, sent_frame_bytes));
        }
        log::debug!(
            target: "mesh_talk::channel",
            "send conv={} members={} seeded={} confirmed={} sent_frame_bytes={} elapsed_ms={}",
            hex::encode(&channel.as_bytes()[..4]),
            members.len(),
            seeded,
            match self.channel_custody_count(channel, event_id) {
                Ok(count) => count.unwrap_or(0),
                Err(error) => { log::warn!("channel send custody observation deferred: {error}"); return; }
            },
            sent_frame_bytes,
            started.elapsed().as_millis(),
        );
    }

    /// Number of distinct other accepted member accounts with historical
    /// authenticated custody evidence. This never establishes delivery or reading.
    /// A sibling-authored row without synchronized evidence remains unknown.
    pub fn channel_custody_count(
        &self,
        channel: ConversationId,
        id: EventId,
    ) -> Result<Option<usize>, NodeError> {
        let physical_member = {
            let book = self.channels.lock().expect("channels mutex");
            book.state(&channel)
                .is_some_and(|state| self.channel_identity_matches_account(state))
        };
        let own_devices: HashSet<_> = self
            .linked_devices()
            .into_iter()
            .map(|device| device.public.user_id())
            .chain(std::iter::once(self.user_id()))
            .collect();
        let own_local = self
            .log
            .lock()
            .expect("log mutex")
            .get(&id)?
            .is_some_and(|event| {
                event.conversation_id == channel
                    && event.kind == EventKind::Message
                    && event.author.user_id() == self.user_id()
            });
        let own_signed = self
            .log
            .lock()
            .expect("log mutex")
            .get(&id)?
            .is_some_and(|event| {
                event.conversation_id == channel
                    && event.kind == EventKind::Message
                    && own_devices.contains(&event.author.user_id())
            });
        let own_archived = self
            .received
            .lock()
            .expect("received mutex")
            .entry(id)?
            .is_some_and(|row| row.conversation == channel && own_devices.contains(&row.from));
        if !own_signed && !own_archived {
            return Ok(None);
        }
        let observations = self.channel_custody_observations(channel, id);
        if !physical_member {
            // Account-private history and signed negatives can establish zero
            // observed external copies without installing a physical grant.
            // Positive holder qualification still needs the existing public
            // membership view; never turn an unresolved positive into zero.
            let (confirmed, _) = self.effective_channel_custody_observations(channel, id)?;
            return Ok((!observations.is_empty()
                && observations.iter().all(|meta| meta.tombstone)
                && confirmed.is_empty()
                && self.account_group_access(channel).is_ok_and(|access| {
                    matches!(
                        access.role,
                        super::AccountGroupRole::Owner | super::AccountGroupRole::Member
                    )
                }))
            .then_some(0));
        }
        if !own_local && observations.is_empty() {
            return Ok(None);
        }
        let (principals, unreadable) = self.confirmed_channel_custody_principals(channel, id)?;
        if unreadable && principals.is_empty() {
            Ok(None)
        } else {
            Ok(Some(principals.len()))
        }
    }

    /// One verified principal rule owns display, retry budgets and recheck peers.
    /// A certificate cannot expand a legacy device-only membership grant.
    pub(in crate::node) fn channel_custody_member_principal(
        &self,
        channel: ConversationId,
        member: &PublicIdentity,
        witnessed_account: Option<String>,
    ) -> Option<String> {
        if *member == self.identity.public()
            || self.certified_sibling(member)
            || self.device_is_post_office(member)
        {
            return None;
        }
        let account = self.account_id_for_device(member).or(witnessed_account);
        if account.as_deref() == Some(self.account_id().as_str()) {
            return None;
        }
        let book = self.channels.lock().expect("channels mutex");
        let state = book.state(&channel)?;
        if !book.replica_enabled(channel, &member.user_id()) {
            return None;
        }
        if state.members().contains(member) && state.is_accepted_member(&member.user_id()) {
            return match state
                .member_account_id(&member.user_id())
                .map(str::to_owned)
                .or(account)
            {
                Some(account) if account != self.account_id() => Some(format!("account:{account}")),
                Some(_) => None,
                None if state.owner_account_id().is_none() => {
                    Some(format!("device:{}", member.user_id()))
                }
                None => None,
            };
        }
        let account = account?;
        (state.owner_account_id().is_some()
            && state.members().iter().any(|accepted| {
                state.is_accepted_member(&accepted.user_id())
                    && state.member_account_id(&accepted.user_id()) == Some(account.as_str())
            }))
        .then(|| format!("account:{account}"))
    }

    fn channel_custody_repair_members(&self, channel: ConversationId) -> Vec<PublicIdentity> {
        let mut members = self
            .channels
            .lock()
            .expect("channels mutex")
            .state(&channel)
            .map(|state| state.members().to_vec())
            .unwrap_or_default();
        for proof in self.historical_peer_proofs() {
            let public = proof.public();
            if !members.contains(&public)
                && self
                    .channel_custody_member_principal(channel, &public, None)
                    .is_some()
            {
                members.push(public);
            }
        }
        members
    }

    fn effective_channel_custody_observations(
        &self,
        channel: ConversationId,
        id: EventId,
    ) -> Result<
        (
            HashSet<String>,
            Vec<super::account_documents::AccountDocumentMeta>,
        ),
        NodeError,
    > {
        let observations = self.channel_custody_observations(channel, id);
        let mut confirmed = self
            .channel_custody
            .lock()
            .expect("custody mutex")
            .peers(id)?;
        for meta in &observations {
            if let super::account_documents::AccountDocumentKey::ChannelCustody { holder, .. } =
                &meta.key
            {
                if meta.tombstone {
                    confirmed.remove(holder);
                } else {
                    confirmed.insert(holder.clone());
                }
            }
        }
        Ok((confirmed, observations))
    }

    fn confirmed_channel_custody_principals(
        &self,
        channel: ConversationId,
        id: EventId,
    ) -> Result<(HashSet<String>, bool), NodeError> {
        let (confirmed, observations) = self.effective_channel_custody_observations(channel, id)?;
        let members = self
            .channels
            .lock()
            .expect("channels mutex")
            .state(&channel)
            .map(|state| state.members().to_vec())
            .unwrap_or_default();
        let unreadable = observations.iter().any(|meta| {
            !meta.tombstone && self.channel_custody_holder_binding(&meta.key).is_none()
        });
        let principals = confirmed.into_iter().filter_map(|device| {
            if device.starts_with("relay:") { return None; }
            if let Some(meta) = observations.iter().find(|meta| !meta.tombstone && matches!(&meta.key, super::account_documents::AccountDocumentKey::ChannelCustody { holder, .. } if holder == &device)) {
                let (holder, account) = self.channel_custody_holder_binding(&meta.key)?;
                return self.channel_custody_member_principal(channel, &holder, account);
            }
            let member = members.iter().find(|member| member.user_id() == device)?;
            self.channel_custody_member_principal(channel, member, None)
        }).collect();
        Ok((principals, unreadable))
    }

    /// Historical exact-wrapper custody confirmed by post offices, separate
    /// from member-copy targets used by recovery and replication policy.
    pub fn channel_post_office_custody_count(
        &self,
        channel: ConversationId,
        id: EventId,
    ) -> Result<Option<usize>, NodeError> {
        if self.channel_custody_count(channel, id)?.is_none() {
            return Ok(None);
        }
        let (confirmed, observations) = self.effective_channel_custody_observations(channel, id)?;
        let unreadable = observations.iter().any(|meta| !meta.tombstone && matches!(&meta.key, super::account_documents::AccountDocumentKey::ChannelCustody { holder, .. } if holder.starts_with("relay:")) && self.channel_custody_holder_binding(&meta.key).is_none());
        if unreadable {
            return Ok(None);
        }
        Ok(Some(
            confirmed
                .iter()
                .filter(|peer| peer.starts_with("relay:"))
                .count(),
        ))
    }

    async fn sync_and_confirm_custody(
        &self,
        channel: ConversationId,
        event_id: EventId,
        peer: &PeerRecord,
    ) -> u64 {
        self.sync_and_confirm_custody_admitted(channel, event_id, peer)
            .await
            .unwrap_or(0)
    }

    async fn sync_and_confirm_custody_admitted(
        &self,
        channel: ConversationId,
        event_id: EventId,
        peer: &PeerRecord,
    ) -> Result<u64, crate::transport::TransportError> {
        let mut conn = self.privacy_sync_dial(peer.addr, &peer.public).await?;
        let before = conn.sent_frame_bytes();
        let store = self.sync_store(conn.peer_identity());
        let reusable = if request_round(&mut conn, &store, channel).await.is_ok() {
            match request_durable_have(&mut conn, channel, event_id).await {
                Ok(held) => {
                    if held {
                        let device = peer.public.user_id();
                        let witnessed = self
                            .publish_channel_custody(
                                channel,
                                event_id,
                                &peer.public,
                                true,
                                false,
                                false,
                            )
                            .is_ok();
                        let persisted = witnessed
                            && self
                                .channel_custody
                                .lock()
                                .expect("channel custody mutex")
                                .confirm(event_id, &device)
                                .is_ok();
                        if persisted {
                            self.delivery_status_notify.notify_waiters();
                        }
                    }
                    true
                }
                Err(_) => false,
            }
        } else {
            false
        };
        let bytes = conn.sent_frame_bytes().saturating_sub(before);
        if reusable {
            self.recycle_sync_channel(conn);
        }
        Ok(bytes)
    }

    /// Periodically verify one old claim without first syncing the missing
    /// event back to that peer. A negative answer retracts the displayed copy;
    /// the normal retry queue may subsequently restore and reconfirm it.
    async fn recheck_one_channel_custody(
        &self,
        cursor: usize,
        after: &mut HashMap<ConversationId, (u64, EventId)>,
        cycles: &mut HashMap<ConversationId, usize>,
    ) {
        let result: Result<(), NodeError> = async {
            let me = self.user_id();
            let author =
                crate::eventlog::event::Author::from_ed25519(self.identity.public().ed25519_pub);
            let mut channels: Vec<_> = {
                let book = self.channels.lock().expect("channels mutex not poisoned");
                book.channel_ids()
                    .into_iter()
                    .filter(|channel| {
                        book.state(channel)
                            .is_some_and(|state| state.is_member(&me) && !state.is_dissolved())
                    })
                    .collect()
            };
            if channels.is_empty() {
                return Ok(());
            }
            channels.sort_unstable_by_key(|channel| *channel.as_bytes());
            let channel = channels[cursor % channels.len()];
            let previous = after.get(&channel).copied();
            let Some((lamport, event_id)) = self
                .log
                .lock()
                .expect("log mutex not poisoned")
                .message_keys_after(channel, author, previous, 1)?
                .into_iter()
                .next()
            else {
                return Ok(());
            };
            let cycle = advance_custody_cursor(channel, (lamport, event_id), after, cycles);
            let (confirmed, _) = self.effective_channel_custody_observations(channel, event_id)?;
            let members = self.channel_custody_repair_members(channel);
            let peers = members
                .iter()
                .filter(|member| {
                    confirmed.contains(&member.user_id())
                        && self
                            .channel_custody_member_principal(channel, member, None)
                            .is_some()
                })
                .filter_map(|member| {
                    self.routing_peer(&member.user_id())
                        .filter(|peer| peer.public == *member)
                })
                .collect();
            let peer = select_custody_peer(peers, cycle);
            let Some(peer) = peer else { return Ok(()) };
            self.recheck_channel_custody_peer(channel, event_id, &peer)
                .await;

            Ok(())
        }
        .await;
        if let Err(error) = result {
            log::warn!("recheck_one_channel_custody deferred after storage failure: {error}");
        }
    }

    pub(in crate::node) async fn recheck_channel_custody_peer(
        &self,
        channel: ConversationId,
        event_id: EventId,
        peer: &PeerRecord,
    ) {
        let check = async {
            let mut conn = self.privacy_sync_dial(peer.addr, &peer.public).await.ok()?;
            let held = request_durable_have(&mut conn, channel, event_id)
                .await
                .ok()?;
            self.recycle_sync_channel(conn);
            Some(held)
        };
        let Ok(Some(held)) = tokio::time::timeout(CHANNEL_RECOVERY_PEER_TIMEOUT, check).await
        else {
            return;
        };
        if self
            .publish_channel_custody(channel, event_id, &peer.public, held, false, false)
            .is_err()
        {
            return;
        }
        let recorded = {
            let mut store = self.channel_custody.lock().expect("channel custody mutex");
            if held {
                store.confirm(event_id, &peer.public.user_id())
            } else {
                store.revoke(event_id, &peer.public.user_id())
            }
        };
        if recorded.is_ok() {
            self.delivery_status_notify.notify_waiters();
        }
    }

    /// Retry one insufficiently replicated own message per tick. The event log is
    /// the durable pending queue, so a restart cannot forget local-only messages.
    /// If no replication exchange starts, return at most one missing-report
    /// candidate for the same recovery slot's read-only durable-have query.
    pub(in crate::node) async fn retry_one_channel_custody(
        &self,
        cursor: usize,
        after: &mut HashMap<ConversationId, (u64, EventId)>,
        cycles: &mut HashMap<ConversationId, usize>,
    ) -> Option<(ConversationId, EventId, PeerRecord)> {
        let result: Result<Option<(ConversationId, EventId, PeerRecord)>, NodeError> = async {
            let me = self.user_id();
            let mut channels: Vec<_> = {
                let book = self.channels.lock().expect("channels mutex not poisoned");
                book.channel_ids()
                    .into_iter()
                    .filter_map(|id| {
                        let state = book.state(&id)?;
                        (state.is_member(&me) && !state.is_dissolved())
                            .then(|| (id, state.members().to_vec()))
                    })
                    .collect()
            };
            if channels.is_empty() {
                return Ok(None);
            }
            channels.sort_unstable_by_key(|(channel, _)| *channel.as_bytes());
            let (channel, _) = &channels[cursor % channels.len()];
            let channel = *channel;
            let members = self.channel_custody_repair_members(channel);
            let author =
                crate::eventlog::event::Author::from_ed25519(self.identity.public().ed25519_pub);
            let candidates: Vec<_> = {
                let log = self.log.lock().expect("log mutex not poisoned");
                log.message_keys_after(
                    channel,
                    author,
                    after.get(&channel).copied(),
                    CHANNEL_CUSTODY_SCAN_PER_TICK,
                )?
            };
            if candidates.is_empty() {
                return Ok(None);
            }
            // A read-only custody report does not require a physical group grant.
            // Pairing's current full-device/account binding selects the endpoint;
            // the ordinary durable-have responder still owns original visibility.
            let own_account = self.account_id();
            let report_peers: Vec<_> = self
                .linked_devices()
                .into_iter()
                .filter(|device| {
                    device.public != self.identity.public()
                        && device.certificate.verify()
                        && device.certificate.account_ed25519_pub
                            == self.account.public().ed25519_pub
                        && device.certificate.device_ed25519_pub == device.public.ed25519_pub
                })
                .filter_map(|device| {
                    self.routing_peer(&device.public.user_id()).filter(|peer| {
                        peer.public == device.public
                            && !peer.post_office
                            && peer.account_id.as_deref() == Some(own_account.as_str())
                    })
                })
                .collect();
            let mut missing_report = None;
            let mut candidates = candidates.into_iter();
            while let Some((event_id, cycle)) =
                next_custody_retry_key(&mut candidates, channel, after, cycles)
            {
                if missing_report.is_none()
                    && self
                        .channel_custody_observations(channel, event_id)
                        .is_empty()
                {
                    missing_report = select_custody_peer(report_peers.clone(), cycle)
                        .map(|peer| (channel, event_id, peer));
                }
                let target = members
                    .iter()
                    .filter_map(|member| {
                        self.channel_custody_member_principal(channel, member, None)
                    })
                    .collect::<HashSet<_>>()
                    .len()
                    .min(2);
                if target == 0
                    || self.channel_custody_count(channel, event_id)?.unwrap_or(0) >= target
                {
                    continue;
                }
                let (principals, _) =
                    self.confirmed_channel_custody_principals(channel, event_id)?;
                let peers = members
                    .iter()
                    .filter(|member| {
                        self.channel_custody_member_principal(channel, member, None)
                            .is_some_and(|principal| !principals.contains(&principal))
                    })
                    .filter_map(|member| {
                        self.routing_peer(&member.user_id())
                            .filter(|peer| peer.public == *member)
                    })
                    .collect();
                if let Some(peer) = select_custody_peer(peers, cycle) {
                    let _ = tokio::time::timeout(
                        CHANNEL_RECOVERY_PEER_TIMEOUT,
                        self.sync_and_confirm_custody(channel, event_id, &peer),
                    )
                    .await;
                    return Ok(None);
                }
            }
            Ok(missing_report)
        }
        .await;
        match result {
            Ok(report) => report,
            Err(error) => {
                log::warn!("channel custody recovery deferred: {error}");
                None
            }
        }
    }

    async fn distribute_channel_targets(
        &self,
        channel: ConversationId,
        members: &[PublicIdentity],
        fanout: Option<usize>,
        replicate_to_post_office: bool,
        confirming: Option<EventId>,
    ) -> bool {
        let result: Result<bool, NodeError> = async {
            let me = self.identity.public().user_id();
            let peers: Vec<PeerRecord> = {
                members
                    .iter()
                    .filter(|m| m.user_id() != me)
                    .filter_map(|m| {
                        self.routing_peer(&m.user_id())
                            .filter(|peer| peer.public == *m)
                    })
                    .collect()
            };
            let targets = if let Some(limit) = fanout {
                let start = self
                    .log
                    .lock()
                    .expect("log mutex not poisoned")
                    .conversation_len(&channel)?
                    .wrapping_add(self.identity.public().ed25519_pub[0] as usize);
                select_channel_targets(peers, members.len(), start, limit)
            } else {
                peers
            };
            // Controls still reach every routed member, but a stalled route must
            // neither block healthy members nor create an unbounded dial burst.
            let deferred = std::sync::atomic::AtomicBool::new(false);
            stream::iter(targets)
                .for_each_concurrent(CHANNEL_CONTROL_FANOUT_CONCURRENCY, |peer| {
                    let deferred = &deferred;
                    async move {
                        let result = tokio::time::timeout(CHANNEL_RECOVERY_PEER_TIMEOUT, async {
                            if let Some(event) = confirming {
                                self.sync_and_confirm_custody_admitted(channel, event, &peer)
                                    .await
                                    .map(|_| ())
                            } else {
                                let mut ch =
                                    self.privacy_sync_dial(peer.addr, &peer.public).await?;
                                let store = self.sync_store(ch.peer_identity());
                                if request_round(&mut ch, &store, channel).await.is_ok() {
                                    self.recycle_sync_channel(ch);
                                }
                                Ok(())
                            }
                        })
                        .await;
                        if let Ok(Err(error)) = result
                            && super::session_pool::local_capacity_busy(&error)
                        {
                            deferred.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                })
                .await;
            if replicate_to_post_office {
                let result = self
                    .replicate_to_post_office_targets_confirming(
                        channel,
                        members
                            .iter()
                            .filter(|member| member.user_id() != me)
                            .cloned()
                            .collect(),
                        confirming,
                    )
                    .await;
                if let Err(super::session::SessionError::Transport(error)) = result
                    && super::session_pool::local_capacity_busy(&error)
                {
                    deferred.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            Ok(deferred.load(std::sync::atomic::Ordering::Relaxed))
        }
        .await;
        match result {
            Ok(deferred) => deferred,
            Err(error) => {
                log::warn!("channel distribution deferred: {error}");
                true
            }
        }
    }

    /// Reconcile one joined channel with an online member. A rotating cursor gives
    /// every channel and member a turn without making the background loop dial the
    /// full group. Any member holding an event can serve it, even if its author is
    /// offline. The ordinary sync store enforces membership and validates events.
    pub(in crate::node) async fn pull_one_channel_from_member(&self, cursor: usize) {
        let channels = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let mut channels: Vec<_> = book
                .channel_ids()
                .into_iter()
                .filter(|id| {
                    book.state(id)
                        .is_some_and(|state| state.is_member(&self.user_id()))
                })
                .collect();
            channels.sort_unstable_by_key(|id| *id.as_bytes());
            channels
        };
        if let Some(channel) = channels.get(cursor % channels.len().max(1)) {
            self.pull_channel_from_members(*channel, cursor / channels.len(), true)
                .await;
        }
    }

    async fn pull_channel_from_members(
        &self,
        channel: ConversationId,
        cursor: usize,
        reconcile_controls: bool,
    ) {
        let channel_id = &channel;
        let me = self.user_id();
        let members = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let Some(state) = book.state(channel_id) else {
                return;
            };
            if !state.is_member(&me) || state.is_dissolved() {
                return;
            }
            state
                .members()
                .iter()
                .filter(|member| {
                    state.is_accepted_member(&member.user_id())
                        && (reconcile_controls || book.replica_enabled(channel, &member.user_id()))
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        // Local projection repair is independent of a reachable remote holder.
        self.process_channel_reception(*channel_id, "catch_up");
        self.process_file_events_reception(*channel_id, "catch_up");
        let peers: Vec<_> = members
            .iter()
            .filter(|member| member.user_id() != me)
            .filter_map(|member| {
                self.routing_peer(&member.user_id())
                    .filter(|peer| peer.public == *member)
            })
            .collect();
        if peers.is_empty() {
            self.record_channel_recovery(*channel_id, "waiting_for_peer", false);
            return;
        }
        self.record_channel_recovery(*channel_id, "catching_up", false);
        // Identity/channel-specific rotation spreads simultaneous gossip wakes
        // across replicas while the cursor still visits every known member.
        let own = self.identity.public();
        let rotation = usize::from(u16::from_le_bytes([
            own.ed25519_pub[0] ^ channel_id.as_bytes()[0],
            own.ed25519_pub[1] ^ channel_id.as_bytes()[1],
        ]));
        let start = rotation.wrapping_add(cursor) % peers.len();
        let mut exchanges = stream::iter(0..peers.len().min(CHANNEL_RECOVERY_PEERS_PER_TICK))
            .map(|offset| {
                let peer = &peers[(start + offset) % peers.len()];
                async move {
                    let result = tokio::time::timeout(CHANNEL_RECOVERY_PEER_TIMEOUT, async {
                        let Ok(mut conn) = self.privacy_sync_dial(peer.addr, &peer.public).await
                        else {
                            return None;
                        };
                        // Recovery fetches retained history; it must not turn an
                        // outbound connection into a sharing-preference bypass.
                        let sharing = match self.shares_replica(channel_id) {
                            Ok(sharing) => sharing,
                            Err(error) => {
                                log::warn!("channel replica export deferred: {error}");
                                return None;
                            }
                        };
                        let store = if sharing {
                            self.sync_store(conn.peer_identity())
                        } else {
                            self.pull_sync_store(conn.peer_identity())
                        };
                        let report = match request_round(&mut conn, &store, *channel_id).await {
                            Ok(report) => report,
                            Err(error) => {
                                log::warn!("channel recovery exchange deferred: {error}");
                                return None;
                            }
                        };
                        self.recycle_sync_channel(conn);
                        Some((report.applied, report.rejected.is_empty()))
                    })
                    .await;
                    if let Ok(Some((applied, clean))) = result {
                        self.process_channel_reception(*channel_id, "catch_up");
                        self.process_file_events_reception(*channel_id, "catch_up");
                        Some((applied, clean))
                    } else {
                        None
                    }
                }
            })
            .buffer_unordered(CHANNEL_RECOVERY_PEERS_PER_TICK);
        let mut checked = false;
        let mut clean = true;
        while let Some(result) = exchanges.next().await {
            if let Some((applied, peer_clean)) = result {
                checked = true;
                clean &= peer_clean;
                if applied > 0 {
                    // Release this group's lane as soon as a source makes
                    // progress. Dropping the remaining futures closes their
                    // uncompleted sessions; subsequent passes rotate sources.
                    break;
                }
            }
        }
        drop(exchanges);
        self.record_channel_recovery(
            channel,
            if !checked {
                "waiting_for_peer"
            } else if clean {
                "peer_checked"
            } else {
                "catching_up"
            },
            checked,
        );
    }
}

#[cfg(test)]
mod fanout_tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::Instant;

    #[tokio::test]
    async fn channel_custody_retry_skips_own_siblings_and_already_confirmed_account() {
        use crate::{
            discovery::Announce,
            identity::account::Account,
            node::{account_devices::AccountDevice, test_support::node_with_files},
        };
        for external in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let account = Account::generate();
            let holder_account = if external {
                Account::generate()
            } else {
                Account::from_secret_bytes(account.secret_bytes())
            };
            let holder = DeviceIdentity::generate();
            let alternate = DeviceIdentity::generate();
            let proof = Announce::new_with_account(&holder, &holder_account, "holder", 9);
            let (node, _, _) = node_with_files(
                dir.path(),
                DeviceIdentity::generate(),
                Account::from_secret_bytes(account.secret_bytes()),
                &proof,
            );
            if !external {
                node.account_devices
                    .lock()
                    .unwrap()
                    .insert(AccountDevice {
                        public: holder.public(),
                        certificate: account.certify(&holder.public().ed25519_pub),
                    })
                    .unwrap();
            }
            let members = if external {
                vec![holder.public(), alternate.public()]
            } else {
                vec![holder.public()]
            };
            let channel = node
                .create_channel("retry custody fixture", members)
                .await
                .unwrap();
            node.send_channel_message(channel, b"synthetic retry target")
                .await
                .unwrap();
            let event = node.channel_history(channel, 10).unwrap()[0].id;
            if external {
                node.publish_channel_custody(channel, event, &holder.public(), true, false, false)
                    .unwrap();
                node.channel_custody
                    .lock()
                    .unwrap()
                    .confirm(event, &holder.public().user_id())
                    .unwrap();
                assert_eq!(node.channel_custody_count(channel, event).unwrap(), Some(1));
            }
            // A newly advertised endpoint accepts TCP but intentionally never
            // completes Noise. Any unnecessary retry is observable at this socket.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            for identity in [&holder, &alternate] {
                node.roster.lock().unwrap().update(
                    &Announce::new_with_account(
                        identity,
                        &holder_account,
                        "holder",
                        address.port(),
                    ),
                    address.ip(),
                    &node.user_id(),
                );
            }
            let report = tokio::time::timeout(
                Duration::from_millis(250),
                node.retry_one_channel_custody(0, &mut HashMap::new(), &mut HashMap::new()),
            )
            .await
            .expect("a satisfied or empty external-principal target must not open a retry");
            assert_eq!(
                report.is_some(),
                !external,
                "only a missing own-sibling report proposes a read-only query; confirmed external custody needs neither work"
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(20), listener.accept())
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn group_recovery_reconnect_resets_idle_backoff_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (messages, _) = tokio::sync::mpsc::unbounded_channel();
        let (file, _) = tokio::sync::mpsc::unbounded_channel();
        let node = Node::open(
            DeviceIdentity::generate(),
            Arc::new(std::sync::Mutex::new(
                crate::discovery::roster::Roster::default(),
            )),
            dm,
            messages,
            file,
            &dir.path().join("node.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        let channel = node
            .create_channel("reconnect", vec![DeviceIdentity::generate().public()])
            .await
            .unwrap();
        let worker = tokio::spawn(
            node.clone()
                .run_channel_recovery_loop(Arc::new(Notify::new())),
        );
        // Observe three distinct timer-driven attempts. The next idle retry is
        // at least twelve seconds away, so a sub-second repair proves the
        // network revision interrupted idle backoff rather than a timer race.
        for _ in 0..3 {
            tokio::time::timeout(Duration::from_secs(10), async {
                while !node.channel_recovery.lock().unwrap().contains_key(&channel) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("bounded idle fallback must run");
            node.channel_recovery.lock().unwrap().clear();
        }
        node.network_changes
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        tokio::time::timeout(Duration::from_millis(500), async {
            while !node.channel_recovery.lock().unwrap().contains_key(&channel) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("network restoration must repair immediately despite idle backoff");
        worker.abort();
    }

    #[tokio::test]
    async fn group_recovery_targets_coalesced_wakes_and_rotates_after_lost_notifications() {
        let dir = tempfile::tempdir().unwrap();
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (messages, _) = tokio::sync::mpsc::unbounded_channel();
        let (file, _) = tokio::sync::mpsc::unbounded_channel();
        let node = Node::open(
            DeviceIdentity::generate(),
            Arc::new(std::sync::Mutex::new(
                crate::discovery::roster::Roster::default(),
            )),
            dm,
            messages,
            file,
            &dir.path().join("node.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        let mut channels = Vec::new();
        for index in 0..12 {
            channels.push(
                node.create_channel(
                    &format!("group {index}"),
                    vec![DeviceIdentity::generate().public()],
                )
                .await
                .unwrap(),
            );
        }
        channels.sort_unstable_by_key(|id| *id.as_bytes());
        // All creation notifications precede subscription. Startup is bounded,
        // and later timer-driven anti-entropy must still visit the other groups.
        let worker = tokio::spawn(
            node.clone()
                .run_channel_recovery_loop(Arc::new(Notify::new())),
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            while node.channel_recovery.lock().unwrap().len() < CHANNEL_RECOVERY_WAKE_BATCH {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let target = channels[11];
        assert!(!node.channel_recovery.lock().unwrap().contains_key(&target));
        for _ in 0..1000 {
            node.schedule_channel_recovery(target);
        }
        assert_eq!(node.channel_recovery_pending.lock().unwrap().len(), 1);
        tokio::time::timeout(Duration::from_millis(500), async {
            while !node.channel_recovery.lock().unwrap().contains_key(&target) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("targeted wake must visit its group before the next rotating timer batch");
        assert!(
            !node
                .channel_recovery
                .lock()
                .unwrap()
                .contains_key(&channels[10])
        );
        tokio::time::timeout(Duration::from_secs(9), async {
            while node.channel_recovery.lock().unwrap().len() < channels.len() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("timer fallback must reconcile groups whose notifications were lost");
        worker.abort();
    }

    #[tokio::test]
    async fn targeted_recovery_skips_known_non_sharing_and_unaccepted_peers() {
        use crate::discovery::announce::Announce;
        use crate::eventlog::event::Event;
        use std::net::IpAddr;
        let dir = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (messages, _) = tokio::sync::mpsc::unbounded_channel();
        let (file, _) = tokio::sync::mpsc::unbounded_channel();
        let node = Node::open(
            DeviceIdentity::generate(),
            Arc::new(std::sync::Mutex::new(
                crate::discovery::roster::Roster::default(),
            )),
            dm,
            messages,
            file,
            &dir.path().join("node.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        let channel = node
            .create_channel("accepted", vec![peer.public()])
            .await
            .unwrap();
        let invited = node
            .create_channel_invited("pending", vec![peer.public()])
            .await
            .unwrap();
        let (parents, lamport) = node.log.lock().unwrap().prepare(&channel).unwrap();
        let preference = Event::new(
            &peer,
            channel,
            1,
            parents,
            lamport,
            1,
            EventKind::ChannelReplica,
            vec![0],
        );
        node.log.lock().unwrap().append(preference).unwrap();
        node.process_channel(channel);
        assert!(!node.channel_replica_enabled(channel, &peer.public().user_id()));
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        node.roster.lock().unwrap().update(
            &Announce::new(&peer, "Peer", listener.local_addr().unwrap().port()),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &node.user_id(),
        );
        for id in [channel, invited] {
            tokio::time::timeout(
                Duration::from_millis(200),
                node.pull_channel_from_members(id, 0, false),
            )
            .await
            .expect("targeted recovery must skip ineligible sources without dialing");
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
        // The lower-frequency control pass must still contact an opted-out peer
        // to discover a later signed re-enable, without needing its push route.
        let probe = tokio::spawn({
            let node = Arc::clone(&node);
            async move {
                node.pull_channel_from_members(channel, 0, true).await;
            }
        });
        tokio::time::timeout(Duration::from_secs(1), listener.accept())
            .await
            .expect("fallback must reconcile preferences")
            .unwrap();
        probe.abort();
    }

    #[tokio::test]
    async fn group_info_changes_publish_once_and_messages_stay_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (messages, _) = tokio::sync::mpsc::unbounded_channel();
        let (file, _) = tokio::sync::mpsc::unbounded_channel();
        let node = Node::open(
            DeviceIdentity::generate(),
            Arc::new(std::sync::Mutex::new(
                crate::discovery::roster::Roster::default(),
            )),
            dm,
            messages,
            file,
            &dir.path().join("node.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        let mut changes = node.subscribe_channel_changes();
        let channel = node.create_channel("original", vec![]).await.unwrap();
        assert_eq!(changes.try_recv().unwrap(), channel);
        node.rename_channel(channel, "renamed").await.unwrap();
        assert_eq!(changes.try_recv().unwrap(), channel);
        node.set_channel_replica_enabled(channel, false)
            .await
            .unwrap();
        assert_eq!(changes.try_recv().unwrap(), channel);
        node.set_channel_replica_enabled(channel, false)
            .await
            .unwrap();
        node.process_channel(channel);
        node.send_channel_message(channel, b"quiet metadata")
            .await
            .unwrap();
        assert!(matches!(
            changes.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        node.dissolve_channel(channel).await.unwrap();
        assert_eq!(changes.try_recv().unwrap(), channel);
    }

    #[test]
    fn group_recovery_fallback_backs_off_with_bounded_jitter_and_resets() {
        let mut backoff = ChannelRecoveryBackoff::default();
        let first = backoff.next_delay(0);
        assert_eq!(first, CHANNEL_RECOVERY_INTERVAL);
        assert!(backoff.next_delay(0) > first);
        for _ in 0..20 {
            backoff.next_delay(255);
        }
        assert!(backoff.next_delay(255) <= CHANNEL_RECOVERY_MAX_INTERVAL);
        backoff.reset();
        assert_eq!(backoff.next_delay(0), first);
    }

    #[tokio::test]
    async fn channel_commit_authorization_rejects_control_and_message_writes() {
        let dir = tempfile::tempdir().unwrap();
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (channel_events, _) = tokio::sync::mpsc::unbounded_channel();
        let (file, _) = tokio::sync::mpsc::unbounded_channel();
        let node = Node::open(
            DeviceIdentity::generate(),
            Arc::new(std::sync::Mutex::new(
                crate::discovery::roster::Roster::default(),
            )),
            dm,
            channel_events,
            file,
            &dir.path().join("node.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        let deny = |_: &mut dyn FnMut() -> Result<(), NodeError>| {
            Err(NodeError::Authorization("session replaced".into()))
        };
        assert!(
            node.create_channel_invited_if("denied", vec![], deny)
                .await
                .is_err()
        );
        assert!(node.list_channels().unwrap().is_empty());
        let channel = node.create_channel("original", vec![]).await.unwrap();
        let before = node.log.lock().unwrap().events(&channel).unwrap().len();
        assert!(
            node.rename_channel_if(channel, "denied", deny)
                .await
                .is_err()
        );
        assert!(node.dissolve_channel_if(channel, deny).await.is_err());
        assert!(
            node.set_channel_replica_enabled_if(channel, false, deny)
                .await
                .is_err()
        );
        assert!(
            node.send_channel_message_reply_expiring_if(channel, b"denied", None, None, deny)
                .await
                .is_err()
        );
        assert!(
            node.send_sticker_channel_expiring_if(channel, "wave", b"denied", None, deny)
                .await
                .is_err()
        );
        assert_eq!(
            node.log.lock().unwrap().events(&channel).unwrap().len(),
            before
        );
        assert_eq!(node.list_channels().unwrap()[0].name, "original");
        assert!(!node.list_channels().unwrap()[0].dissolved);
    }

    #[test]
    fn group_reception_recovery_observations_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (channel, _) = tokio::sync::mpsc::unbounded_channel();
        let (file, _) = tokio::sync::mpsc::unbounded_channel();
        let node = Node::open(
            DeviceIdentity::generate(),
            Arc::new(std::sync::Mutex::new(
                crate::discovery::roster::Roster::default(),
            )),
            dm,
            channel,
            file,
            &dir.path().join("node.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        for id in 0..4100u64 {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&id.to_be_bytes());
            node.record_channel_recovery(ConversationId::new(bytes), "waiting_for_peer", false);
        }
        assert_eq!(node.channel_recovery.lock().unwrap().len(), 4096);
    }

    #[test]
    fn large_channel_limits_and_rotates_initial_targets() {
        let peers: Vec<_> = (0..5)
            .map(|port| PeerRecord {
                public: DeviceIdentity::generate().public(),
                addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 4000 + port)),
                routes: Vec::new(),
                name: String::new(),
                post_office: false,
                account_id: None,
                last_seen: Instant::now(),
            })
            .collect();
        let first = select_channel_targets(peers.clone(), 6, 0, 2);
        let next = select_channel_targets(peers.clone(), 6, 1, 2);
        assert_eq!(first.len(), 2);
        assert_eq!(next.len(), 2);
        assert_ne!(first[0].public, next[0].public);
        assert_eq!(select_channel_targets(peers.clone(), 5, 0, 2).len(), 5);
    }

    #[test]
    fn custody_recheck_rotates_confirmed_peers_after_each_message_cycle() {
        let channel = ConversationId::new([9; 32]);
        let key = (1, EventId::new([7; 32]));
        let mut after = HashMap::new();
        let mut cycles = HashMap::new();
        let peers: Vec<_> = (0..2)
            .map(|port| PeerRecord {
                public: DeviceIdentity::generate().public(),
                addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 4100 + port)),
                routes: Vec::new(),
                name: String::new(),
                post_office: false,
                account_id: None,
                last_seen: Instant::now(),
            })
            .collect();
        let first_cycle = advance_custody_cursor(channel, key, &mut after, &mut cycles);
        let second_cycle = advance_custody_cursor(channel, key, &mut after, &mut cycles);
        let first = select_custody_peer(peers.clone(), first_cycle).unwrap();
        let second = select_custody_peer(peers, second_cycle).unwrap();
        assert_eq!(first_cycle, 0);
        assert_eq!(second_cycle, 1);
        assert_ne!(first.public, second.public);
    }

    #[test]
    fn custody_retry_rotates_after_a_failed_first_candidate() {
        let channel = ConversationId::new([8; 32]);
        let last_in_batch = (2, EventId::new([8; 32]));
        let mut after = HashMap::new();
        let mut cycles = HashMap::new();
        let peers: Vec<_> = (0..2)
            .map(|port| PeerRecord {
                public: DeviceIdentity::generate().public(),
                addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 4200 + port)),
                routes: Vec::new(),
                name: String::new(),
                post_office: false,
                account_id: None,
                last_seen: Instant::now(),
            })
            .collect();
        let first = select_custody_peer(
            peers.clone(),
            advance_custody_cursor(channel, last_in_batch, &mut after, &mut cycles),
        )
        .unwrap();
        let second = select_custody_peer(
            peers,
            advance_custody_cursor(channel, last_in_batch, &mut after, &mut cycles),
        )
        .unwrap();
        assert_ne!(first.public, second.public);
    }

    #[test]
    fn failed_retry_does_not_starve_a_later_message_in_the_same_batch() {
        let channel = ConversationId::new([6; 32]);
        let first = (1, EventId::new([1; 32]));
        let later = (2, EventId::new([2; 32]));
        let mut after = HashMap::new();
        let mut cycles = HashMap::new();

        // The first attempt fails and this tick stops. Only the examined key
        // should be committed; the next tick must start at the later message.
        let mut first_tick = [first, later].into_iter();
        assert_eq!(
            next_custody_retry_key(&mut first_tick, channel, &mut after, &mut cycles),
            Some((first.1, 0))
        );
        assert_eq!(after.get(&channel), Some(&first));
        let mut next_tick = [later, first].into_iter();
        assert_eq!(
            next_custody_retry_key(&mut next_tick, channel, &mut after, &mut cycles),
            Some((later.1, 0))
        );
        assert_eq!(after.get(&channel), Some(&later));
    }
}
