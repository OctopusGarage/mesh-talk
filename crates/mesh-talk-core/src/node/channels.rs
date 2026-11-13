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
use std::collections::{BTreeSet, HashMap, HashSet};
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

/// Scheduling evidence only: no pending row or cached authority grants progress.
#[derive(Default)]
pub(super) struct LocalRecoveryWork {
    pub(super) more: bool,
    pub(super) retry_after: Option<Duration>,
}
impl LocalRecoveryWork {
    fn merge(&mut self, other: Self) {
        self.more |= other.more;
        self.retry_after = match (self.retry_after, other.retry_after) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
}
const CHANNEL_RECOVERY_QUIET_INTERVAL: Duration = Duration::from_secs(300);

/// Bounded peer children retain participant admission through actual retirement.
/// A completed applied page cancels and joins the remaining sessions.
pub(super) async fn complete_channel_peer_rounds<F>(
    node: &Arc<Node>,
    rounds: impl IntoIterator<Item = F>,
) -> (bool, bool, bool)
where
    F: std::future::Future<Output = Option<(usize, bool)>> + Send + 'static,
{
    let mut rounds = rounds.into_iter();
    let mut children = tokio::task::JoinSet::new();
    let mut checked = false;
    let mut applied_progress = false;
    let mut clean = true;
    let mut closed = false;
    let mut failure = None;
    loop {
        while !closed && children.len() < CHANNEL_RECOVERY_PEERS_PER_TICK {
            let Some(round) = rounds.next() else { break };
            let Some(permit) = node.runtime_work.admit() else {
                closed = true;
                children.abort_all();
                break;
            };
            children.spawn(permit.track(round));
        }
        let Some(result) = children.join_next().await else {
            break;
        };
        match result {
            Ok(Some((applied, peer_clean))) => {
                checked = true;
                clean &= peer_clean;
                if applied > 0 {
                    applied_progress = true;
                    break;
                }
            }
            Ok(None) => {}
            Err(error) if closed && error.is_cancelled() => {}
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    // Abort is a request, not retirement. A first applied page keeps this
    // group's lane until every cancelled child has actually dropped its scope.
    children.abort_all();
    while let Some(result) = children.join_next().await {
        if let Err(error) = result
            && !error.is_cancelled()
            && failure.is_none()
        {
            failure = Some(error);
        }
    }
    if let Some(error) = failure {
        if error.is_panic() {
            std::panic::resume_unwind(error.into_panic());
        }
        panic!("owned channel peer child was unexpectedly cancelled");
    }
    (checked, clean, applied_progress)
}

/// Select a bounded lexical successor batch. A hot requeued low ID cannot
/// displace later pending groups, even when the previously served ID was removed.
fn take_channel_recovery_targets(
    pending: &mut HashSet<ConversationId>,
    last_served: &mut Option<(ConversationId, usize)>,
) -> Vec<(ConversationId, usize)> {
    let mut targets: Vec<_> = pending.iter().copied().collect();
    targets.sort_unstable_by_key(|id| *id.as_bytes());
    if let Some((last, _)) = last_served {
        let next = targets
            .iter()
            .position(|id| id.as_bytes() > last.as_bytes())
            .unwrap_or(0);
        targets.rotate_left(next);
    }
    targets.truncate(CHANNEL_RECOVERY_WAKE_BATCH);
    let mut selected = Vec::with_capacity(targets.len());
    for target in targets {
        // Advance once per lexical sweep, including a wrap inside a batch.
        // Advancing per batch aliases peer rotation when group/batch counts
        // share a divisor with the number of peers in a group.
        let cycle = last_served.as_ref().map_or(0, |(last, cycle)| {
            cycle.wrapping_add(usize::from(target.as_bytes() <= last.as_bytes()))
        });
        *last_served = Some((target, cycle));
        pending.remove(&target);
        selected.push((target, cycle));
    }
    selected
}

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
type ChannelControlDeadlines = HashMap<([u8; 32], [u8; 32]), Option<Instant>>;

/// One finite replay sweep. These are scheduling cursors, never remote ACKs.
struct ChannelControlReplay {
    active: bool,
    upper: Option<[u8; 32]>,
    after: Option<[u8; 32]>,
    resweep: bool,
    failed: BTreeSet<[u8; 32]>,
    retrying: bool,
    quiet: bool,
    at: tokio::time::Instant,
    backoff: ChannelRecoveryBackoff,
}
impl ChannelControlReplay {
    fn new() -> Self {
        Self {
            active: false,
            upper: None,
            after: None,
            resweep: false,
            failed: BTreeSet::new(),
            retrying: false,
            quiet: false,
            at: tokio::time::Instant::now(),
            backoff: ChannelRecoveryBackoff::default(),
        }
    }
    fn invalidate(&mut self) {
        if self.active {
            self.resweep = true;
        } else {
            self.retrying = false;
            self.at = tokio::time::Instant::now();
            self.quiet = false;
        }
    }
}

/// Independent retained inputs share the canonical recovery parent's task.
/// Remembered leases are scheduling evidence only, never dial authorization.
struct ChannelControlInputs {
    metadata: tokio::sync::broadcast::Receiver<ConversationId>,
    roster: tokio::sync::watch::Receiver<u64>,
    network: tokio::sync::watch::Receiver<u64>,
    availability: tokio::sync::watch::Receiver<u64>,
    maintenance: tokio::sync::watch::Receiver<u64>,
    input: [u8; 32],
    deadlines: ChannelControlDeadlines,
}
impl ChannelControlInputs {
    fn new(node: &Node) -> Self {
        let metadata = node.subscribe_channel_changes();
        let roster = node.subscribe_roster_changes();
        let network = node.network_changes.subscribe();
        let availability = node.subscribe_sibling_availability();
        let maintenance = node.storage.database().subscribe_maintenance_changes();
        let mut deadlines = HashMap::new();
        let (input, _) = node.channel_control_route_input(&mut deadlines);
        Self {
            metadata,
            roster,
            network,
            availability,
            maintenance,
            input,
            deadlines,
        }
    }
    async fn changed(&mut self, node: &Node) -> Option<bool> {
        let force = tokio::select! {
            change = self.metadata.recv() => match change {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            },
            change = self.network.changed() => { change.ok()?; true },
            change = self.roster.changed() => { change.ok()?; false },
            change = self.availability.changed() => { change.ok()?; false },
            change = self.maintenance.changed() => { change.ok()?; false },
        };
        let (input, renewed) = node.channel_control_route_input(&mut self.deadlines);
        let changed = force || renewed || input != self.input;
        self.input = input;
        Some(changed)
    }
}

#[derive(Default)]
struct ChannelDistributionOutcome {
    deferred: bool,
    failed: bool,
}

const SMALL_CHANNEL_MEMBER_LIMIT: usize = 5;
const LARGE_CHANNEL_INITIAL_FANOUT: usize = 2;
const CHANNEL_CONTROL_FANOUT_CONCURRENCY: usize = 4;
const CHANNEL_CUSTODY_SCAN_PER_TICK: usize = 128;
const CHANNEL_CUSTODY_RECHECK_INTERVAL: Duration = Duration::from_secs(60);
const CHANNEL_CUSTODY_QUIET_INTERVAL: Duration = Duration::from_secs(300);

#[derive(Default)]
struct CustodyRetryProgress {
    quiet_complete: bool,
}

// One owner replaces the existing parallel retry position/cycle maps. Sweep
// qualification adds fixed scalar fields per existing channel cursor, not a new
// history- or channel-cardinality cache. Physical position survives invalidation.
#[derive(Default)]
struct CustodyRetryCursor {
    after: Option<(u64, EventId)>,
    cycle: usize,
    generation: u64,
    boundary: Option<(u64, EventId)>,
    wraps: u8,
    complete: bool,
}
impl CustodyRetryCursor {
    fn restart_sweep(&mut self) {
        self.boundary = self.after;
        self.wraps = 0;
        self.complete = false;
    }
    fn advance(&mut self, key: (u64, EventId)) -> (usize, bool) {
        if self.after.is_some_and(|old| key <= old) {
            self.cycle = self.cycle.wrapping_add(1);
            self.wraps = self.wraps.saturating_add(1);
        }
        self.after = Some(key);
        let covered = self.wraps > 0
            && (self.boundary.is_none()
            || self.boundary.is_some_and(|boundary| key >= boundary)
            // A removed boundary can lie above every surviving key.
            || self.wraps > 1);
        (self.cycle, covered)
    }
}

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

#[cfg(test)]
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

// Two competing work classes share the existing recovery slot. Advance peer
// rotation once per pair, so alternating classes cannot skip every other peer.
fn custody_retry_order(cycle: usize) -> (bool, usize) {
    (!cycle.is_multiple_of(2), cycle / 2)
}

/// Local bounded recovery observations. A successful peer exchange never proves
/// that offline members have no additional events.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChannelRecovery {
    pub phase: &'static str,
    pub checked_at: Option<u64>,
}

/// Only intrinsic signature results survive between rows in one bounded request.
/// Membership, document bindings and policy are always read by their owners.
#[derive(Hash, PartialEq, Eq)]
struct CustodyCertificateKey {
    account: [u8; 32],
    device: [u8; 32],
    signature: [u8; 64],
}

type ChannelCustodyCounts = Vec<(Option<usize>, Option<usize>)>;

#[derive(Default)]
pub(in crate::node) struct CustodyCertificateVerifier {
    verified: HashMap<CustodyCertificateKey, bool>,
}

impl CustodyCertificateVerifier {
    pub(in crate::node) fn verify(
        &mut self,
        node: &Node,
        holder: &PublicIdentity,
        certificate: &crate::identity::account::DeviceCertificate,
    ) -> bool {
        let key = CustodyCertificateKey {
            account: certificate.account_ed25519_pub,
            device: certificate.device_ed25519_pub,
            signature: certificate.signature,
        };
        if let Some(result) = self.verified.get(&key) {
            return *result;
        }
        #[cfg(test)]
        node.trace_account_client(holder, || "custody_holder_certificate_verify".into());
        #[cfg(not(test))]
        let _ = (node, holder);
        let result = certificate.verify();
        if self.verified.len() < 256 {
            self.verified.insert(key, result);
        }
        result
    }
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
        let periodic = true;
        let maintenance = Arc::clone(&self).run_account_channel_maintenance_loop(periodic);
        Box::pin(self.run_channel_recovery_with_maintenance(changes, periodic, maintenance)).await;
    }

    fn channel_recovery_periodic_enabled(&self, periodic: bool) -> bool {
        // Tests may finish cold recovery before measuring notification-only work.
        // An explicitly disabled worker remains disabled throughout its lifetime.
        #[cfg(test)]
        {
            periodic
                && !self
                    .channel_recovery_periodic_disabled
                    .load(std::sync::atomic::Ordering::Relaxed)
        }
        #[cfg(not(test))]
        {
            periodic
        }
    }

    /// One parent owns all four futures; cancellation drops them without detached work.
    pub(super) async fn run_channel_recovery_with_maintenance(
        self: Arc<Self>,
        changes: Arc<Notify>,
        periodic: bool,
        maintenance: impl std::future::Future<Output = ()>,
    ) {
        let custody = Arc::clone(&self).run_channel_custody_recovery_loop(periodic);
        let controls = Arc::clone(&self).run_channel_control_recovery_loop(periodic);
        tokio::select! {
            _ = Box::pin(controls) => {}
            _ = Box::pin(self.run_local_channel_recovery_loop(changes, periodic)) => {}
            _ = maintenance => {}
            _ = custody => {}
        }
    }

    async fn run_account_channel_maintenance_loop(self: Arc<Self>, periodic: bool) {
        let mut work = super::channel_account_maintenance::ChannelAccountMaintenance::default();
        // These subscriptions do not consume the local recovery/roster Notify
        // permits. The existing maintenance state survives every local wake.
        let mut metadata = self.subscribe_channel_changes();
        let mut network = self.network_changes.subscribe();
        let mut ui = self.subscribe_ui_changes();
        let mut documents = self.account_document_updates.subscribe();
        let mut roster = self.subscribe_roster_changes();
        let mut availability = self.subscribe_sibling_availability();
        loop {
            let periodic_wake = tokio::select! {
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(work.deadline())),
                    if self.channel_recovery_periodic_enabled(periodic) => true,
                change = availability.changed() => { if change.is_err() { return; } false },
                change = ui.changed() => { if change.is_err() { return; } false },
                change = documents.changed() => { if change.is_err() { return; } false },
                change = roster.changed() => { if change.is_err() { return; } false },
                change = metadata.recv() => match change {
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => false,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                change = network.changed() => {
                    if change.is_err() { return; }
                    false
                },
            };
            // The test policy can change while the selected timer is pending.
            if periodic_wake && !self.channel_recovery_periodic_enabled(periodic) {
                continue;
            }
            #[cfg(test)]
            if periodic_wake {
                self.trace_account_client(&self.identity.public(), || {
                    "channel_recovery_maintenance_periodic_begin".into()
                });
            }
            #[cfg(test)]
            self.trace_account_client(&self.identity.public(), || {
                "channel_recovery_maintenance_begin".into()
            });
            self.maintain_account_channels(&mut work).await;
            #[cfg(test)]
            self.trace_account_client(&self.identity.public(), || {
                "channel_recovery_maintenance_done".into()
            });
            tokio::task::yield_now().await;
        }
    }

    fn note_local_channel_recovery_input(&self) {
        self.note_channel_plaintext_recovery_input();
        self.note_group_metadata_recovery_input();
        self.note_join_history_recovery_input();
    }

    fn prepare_local_channel_recovery_turn(&self) -> LocalRecoveryWork {
        let work = match self.retry_channel_plaintext_turn() {
            Ok(work) => work,
            Err(error) => {
                log::warn!("channel work discovery deferred: {error}");
                #[cfg(test)]
                self.trace_account_client(&self.identity.public(), || {
                    "channel_recovery_local_failure".into()
                });
                LocalRecoveryWork {
                    more: false,
                    retry_after: Some(CHANNEL_RECOVERY_INTERVAL),
                }
            }
        };
        #[cfg(test)]
        self.trace_account_client(&self.identity.public(), || {
            "channel_recovery_plaintext_done".into()
        });
        work
    }

    /// Keep one remote turn and its leases alive while notified local work advances.
    /// The outer selection is inactive here, so this Notify has one consumer.
    async fn complete_channel_recovery_turn(
        &self,
        mut remote: std::pin::Pin<&mut impl std::future::Future<Output = LocalRecoveryWork>>,
    ) -> LocalRecoveryWork {
        let mut work = LocalRecoveryWork::default();
        let mut consumed_notification = false;
        loop {
            tokio::select! {
                result = remote.as_mut() => { work.merge(result); break; },
                () = self.channel_recovery_notify.notified() => {
                    consumed_notification = true;
                    self.note_local_channel_recovery_input();
                    work.merge(self.prepare_local_channel_recovery_turn());
                    tokio::task::yield_now().await;
                }
            }
        }
        // A local notification may also own a pending network target. Preserve
        // one outer turn without restarting the active exchange or adding a queue.
        if consumed_notification {
            self.channel_recovery_notify.notify_one();
        }
        work
    }

    // Independent subscriptions never consume the local recovery Notify permit.
    fn channel_custody_work_input(&self) -> Result<[u8; 32], LogError> {
        use sha2::{Digest, Sha256};
        let projection = self.group_projection_revision()?;
        let custody = self
            .channel_custody
            .lock()
            .expect("custody mutex")
            .revision();
        let now = Instant::now();
        let mut peers: Vec<_> = self
            .roster
            .lock()
            .expect("roster mutex")
            .peers()
            .into_iter()
            .map(|peer| {
                let routes = peer.scheduling_endpoints();
                let online = peer.presence_deadline(now).is_some_and(|at| at > now);
                (
                    peer.public,
                    peer.account_id,
                    peer.post_office,
                    routes,
                    online,
                )
            })
            .collect();
        peers.sort_unstable_by_key(|peer| (peer.0.ed25519_pub, peer.0.x25519_pub));
        let bytes = bincode::serialize(&(
            projection,
            custody,
            peers,
            self.group_file_sharing_status().paused,
            self.sibling_availability_revision(),
        ))
        .map_err(|error| LogError::Serialization(error.to_string()))?;
        Ok(Sha256::digest(bytes).into())
    }

    async fn run_channel_custody_recovery_loop(self: Arc<Self>, periodic: bool) {
        let mut metadata = self.subscribe_channel_changes();
        let mut network = self.network_changes.subscribe();
        let mut ui = self.subscribe_ui_changes();
        let mut documents = self.account_document_updates.subscribe();
        let mut roster = self.subscribe_roster_changes();
        let mut availability = self.subscribe_sibling_availability();
        let mut source = self.storage.database().subscribe_source_changes();
        let mut custody_cursor = 0usize;
        let mut recheck_cursor = 0usize;
        let mut retry_cursors = HashMap::<ConversationId, CustodyRetryCursor>::new();
        let mut generation = 1u64;
        let mut recheck_after = HashMap::new();
        let mut recheck_cycle = HashMap::new();
        let mut input = None;
        let mut settled = false;
        let mut retry_at = tokio::time::Instant::now();
        let mut recheck_at = retry_at;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(retry_at.min(recheck_at)) => {},
                change = ui.changed() => { if change.is_err() { return; } },
                change = source.changed() => { if change.is_err() { return; } },
                change = documents.changed() => { if change.is_err() { return; } },
                change = roster.changed() => { if change.is_err() { return; } },
                change = network.changed() => { if change.is_err() { return; } input = None; },
                change = availability.changed() => { if change.is_err() { return; } },
                change = metadata.recv() => match change {
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {},
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
            }
            if !self.channel_recovery_periodic_enabled(periodic) {
                retry_at = tokio::time::Instant::now() + CHANNEL_RECOVERY_INTERVAL;
                recheck_at = retry_at;
                continue;
            }
            let observed = self.channel_custody_work_input().ok();
            if observed.is_none()
                || observed != input
                || (settled && tokio::time::Instant::now() >= retry_at)
            {
                generation = match generation.checked_add(1) {
                    Some(next) => next,
                    None => {
                        for state in retry_cursors.values_mut() {
                            state.generation = 0;
                        }
                        1
                    }
                };
                settled = false;
                retry_at = tokio::time::Instant::now();
            }
            if tokio::time::Instant::now() >= retry_at {
                #[cfg(test)]
                self.trace_account_client(&self.identity.public(), || {
                    "channel_recovery_custody_begin".into()
                });
                let mut progress = CustodyRetryProgress::default();
                if let Some((channel, event, peer)) = self
                    .retry_channel_custody_turn(
                        custody_cursor,
                        &mut retry_cursors,
                        generation,
                        &mut progress,
                    )
                    .await
                {
                    self.recheck_channel_custody_peer(channel, event, &peer)
                        .await;
                }
                custody_cursor = custody_cursor.wrapping_add(1);
                // A captured pre-work input cannot acknowledge a concurrent or
                // self-produced update. Its retained subscription wakes the owner.
                input = observed;
                settled = input.is_some() && progress.quiet_complete;
                retry_at = tokio::time::Instant::now()
                    + if settled {
                        CHANNEL_CUSTODY_QUIET_INTERVAL
                    } else {
                        CHANNEL_RECOVERY_INTERVAL
                    };
                #[cfg(test)]
                self.trace_account_client(&self.identity.public(), || {
                    "channel_recovery_custody_done".into()
                });
            }
            if tokio::time::Instant::now() >= recheck_at {
                #[cfg(test)]
                self.trace_account_client(&self.identity.public(), || {
                    "channel_recovery_custody_recheck_begin".into()
                });
                self.recheck_one_channel_custody(
                    recheck_cursor,
                    &mut recheck_after,
                    &mut recheck_cycle,
                )
                .await;
                recheck_cursor = recheck_cursor.wrapping_add(1);
                recheck_at = tokio::time::Instant::now() + CHANNEL_CUSTODY_RECHECK_INTERVAL;
            }
            tokio::task::yield_now().await;
        }
    }

    // Raw observations do not grant replay credit. Canonical routing facts,
    // actual availability and privacy changes do; each dial still authorizes now.
    fn channel_control_route_input(
        &self,
        deadlines: &mut ChannelControlDeadlines,
    ) -> ([u8; 32], bool) {
        use sha2::{Digest, Sha256};
        let now = Instant::now();
        let mut current_deadlines = HashMap::new();
        let mut renewed = false;
        let mut peers: Vec<_> = self
            .roster
            .lock()
            .expect("roster mutex")
            .peers()
            .into_iter()
            .map(|peer| {
                let endpoints = peer.scheduling_endpoints();
                let identity = (peer.public.ed25519_pub, peer.public.x25519_pub);
                let deadline = peer.presence_deadline(now);
                // Same rule as FanoutRoutes: refresh remembered leases without
                // granting ordinary heartbeats retry credit. An expired lease
                // followed by fresh evidence is a new availability edge even
                // when no intermediate observation saw the peer offline.
                renewed |= deadlines.get(&identity).is_some_and(|old| {
                    old.is_none_or(|at| at <= now) && deadline.is_some_and(|at| at > now)
                });
                current_deadlines.insert(identity, deadline);
                let online = deadline.is_some();
                (
                    peer.public,
                    peer.account_id,
                    peer.post_office,
                    endpoints,
                    online,
                )
            })
            .collect();
        peers.sort_unstable_by_key(|peer| (peer.0.ed25519_pub, peer.0.x25519_pub));
        let (mut routes, proof_revision) = self
            .privacy
            .state
            .read()
            .expect("privacy state")
            .as_ref()
            .map(|state| (state.routes.routes(), Some(state.proofs.revision())))
            .unwrap_or_default();
        routes.sort_unstable_by_key(|(public, ip)| (public.ed25519_pub, public.x25519_pub, *ip));
        let bytes = bincode::serialize(&(
            peers,
            routes,
            proof_revision,
            self.privacy
                .generation
                .load(std::sync::atomic::Ordering::Acquire),
            self.sibling_availability_revision(),
        ))
        .expect("routing scheduling input serializes");
        *deadlines = current_deadlines;
        (Sha256::digest(bytes).into(), renewed)
    }

    async fn replay_channel_controls_turn(&self, replay: &mut ChannelControlReplay) {
        if !replay.active {
            replay.upper = if replay.retrying {
                replay.failed.last().copied()
            } else {
                self.channels
                    .lock()
                    .expect("channels mutex")
                    .control_replay_upper()
            };
            replay.after = None;
            replay.active = true;
        }
        let (page, _, mut more) = replay
            .upper
            .map(|upper| {
                if replay.retrying {
                    use std::ops::Bound::{Excluded, Included, Unbounded};
                    let mut rows = replay
                        .failed
                        .range((replay.after.map_or(Unbounded, Excluded), Included(upper)));
                    let ids: Vec<_> = rows.by_ref().take(32).copied().collect();
                    let more = rows.next().is_some();
                    let last = ids.last().copied().or(replay.after);
                    (
                        ids.into_iter().map(ConversationId::new).collect(),
                        last,
                        more,
                    )
                } else {
                    self.channels
                        .lock()
                        .expect("channels mutex")
                        .control_replay_page(replay.after, upper)
                }
            })
            .unwrap_or_default();
        #[cfg(test)]
        self.trace_account_client(&self.identity.public(), || {
            format!("channel_control_retry_selected count={}", page.len())
        });
        let mut after = replay.after;
        let page_len = page.len();
        for (offset, channel) in page.into_iter().enumerate() {
            after = Some(*channel.as_bytes());
            // Do not retain a recipient/authority view while preparation awaits.
            if self
                .channels
                .lock()
                .expect("channels mutex")
                .pending_control_state(channel, &self.user_id())
                .is_none()
            {
                replay.failed.remove(channel.as_bytes());
                continue;
            }
            // One preparation/distribution per owner turn. Unvisited book rows
            // stay behind this cursor, so other local/remote lanes get a turn.
            more |= offset + 1 < page_len;
            if let Err(error) = self.await_channel_metadata(channel).await {
                log::warn!("channel replay preparation deferred: {error}");
                replay.failed.insert(*channel.as_bytes());
                break;
            }
            let members = self
                .channels
                .lock()
                .expect("channels mutex")
                .pending_control_members(channel, &self.user_id());
            if let Some(members) = members {
                self.observe_distribution_origin(std::panic::Location::caller(), false);
                let outcome = self
                    .distribute_channel_targets_outcome(channel, &members, None, true, None)
                    .await;
                if outcome.failed || outcome.deferred {
                    replay.failed.insert(*channel.as_bytes());
                } else {
                    replay.failed.remove(channel.as_bytes());
                }
                #[cfg(test)]
                self.trace_account_client(&self.identity.public(), || {
                    format!("channel_control_retry_distributed {channel:?}")
                });
            } else {
                replay.failed.remove(channel.as_bytes());
            }
            break;
        }
        replay.after = after;
        if more {
            replay.at = tokio::time::Instant::now();
            return;
        }
        replay.active = false;
        if std::mem::take(&mut replay.resweep) {
            // Do not acknowledge a notification received during the captured cut.
            replay.retrying = false;
            replay.at = tokio::time::Instant::now();
            replay.quiet = false;
        } else if !replay.failed.is_empty() {
            replay.retrying = true;
            replay.at = tokio::time::Instant::now() + replay.backoff.next_delay(0);
            replay.quiet = false;
        } else {
            replay.retrying = false;
            replay.backoff.reset();
            replay.at = tokio::time::Instant::now() + CHANNEL_RECOVERY_QUIET_INTERVAL;
            replay.quiet = true;
        }
        #[cfg(test)]
        self.trace_account_client(&self.identity.public(), || {
            "channel_control_retry_complete".into()
        });
    }

    async fn run_channel_control_recovery_loop(self: Arc<Self>, periodic: bool) {
        let mut replay = ChannelControlReplay::new();
        let mut inputs = ChannelControlInputs::new(&self);
        loop {
            if tokio::time::Instant::now() >= replay.at
                && (!replay.quiet || self.channel_recovery_periodic_enabled(periodic))
            {
                let mut dirty = false;
                {
                    let step = self.replay_channel_controls_turn(&mut replay);
                    tokio::pin!(step);
                    loop {
                        tokio::select! {
                            () = &mut step => break,
                            changed = inputs.changed(&self) => match changed {
                                Some(changed) => dirty |= changed,
                                None => return,
                            },
                        }
                    }
                }
                // Inputs are captured before/while work, never acknowledged by
                // its completion. Preserve one follow-up without restarting I/O.
                if dirty {
                    replay.invalidate();
                }
                tokio::task::yield_now().await;
            } else {
                tokio::select! {
                    _ = tokio::time::sleep_until(replay.at),
                        if !replay.quiet || self.channel_recovery_periodic_enabled(periodic) => {},
                    changed = inputs.changed(&self) => match changed {
                        Some(true) => replay.invalidate(),
                        Some(false) => {},
                        None => return,
                    },
                }
            }
        }
    }

    async fn run_local_channel_recovery_loop(
        self: Arc<Self>,
        changes: Arc<Notify>,
        periodic: bool,
    ) {
        let mut cursor = 0usize;
        let mut last_target = None;
        let mut fallback_remaining = 0usize;
        let mut fallback_failed = false;
        let mut fallback_resweep = false;
        let mut fallback_retry = false;
        let node = &self;
        let mut metadata = self.subscribe_channel_changes();
        let mut network = self.network_changes.subscribe();
        let mut backoff = ChannelRecoveryBackoff::default();
        let mut fallback_at = tokio::time::Instant::now();
        let mut local_at = Some(fallback_at);
        let mut local_continuation = false;
        let mut membership_needed = true;
        let mut history_retry_at = None;
        let mut metadata_retry_at = None;
        let mut membership_retry_at = None;
        loop {
            let mut discovered = false;
            let mut input = true;
            tokio::select! {
                _ = changes.notified() => { discovered = true; },
                _ = network.changed() => { discovered = true; },
                _ = self.channel_recovery_notify.notified() => {},
                change = metadata.recv() => match change {
                    Ok(channel) => self.schedule_channel_recovery(channel),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => discovered = true,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                _ = tokio::time::sleep_until(local_at.unwrap_or_else(tokio::time::Instant::now)), if local_at.is_some() => { input = !local_continuation; },
                _ = tokio::time::sleep_until(fallback_at), if self.channel_recovery_periodic_enabled(periodic) || fallback_retry => { input = fallback_remaining == 0; },
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
            if input {
                self.note_local_channel_recovery_input();
                membership_needed = true;
                history_retry_at = None;
                metadata_retry_at = metadata_retry_at.map(|_| tokio::time::Instant::now());
                membership_retry_at = None;
            }
            let mut work = self.prepare_local_channel_recovery_turn();
            let remote = async {
                let mut result = LocalRecoveryWork::default();
                let mut failure_delay = None;
                if history_retry_at.is_none_or(|at| tokio::time::Instant::now() >= at) {
                    history_retry_at = None;
                    match self.retry_join_history_owned() {
                        Ok((channels, more)) => {
                            result.more |= more;
                            for channel in channels {
                                let members = self.channel_members(channel);
                                self.distribute_channel(channel, &members).await;
                            }
                        }
                        Err(error) => {
                            log::warn!("channel history recovery deferred: {error}");
                            history_retry_at =
                                Some(tokio::time::Instant::now() + CHANNEL_RECOVERY_INTERVAL);
                        }
                    }
                }
                #[cfg(test)]
                self.trace_account_client(&self.identity.public(), || {
                    "channel_recovery_join_history_done".into()
                });
                if metadata_retry_at.is_none_or(|at| tokio::time::Instant::now() >= at) {
                    let recovering = metadata_retry_at.take().is_some();
                    match self.retry_group_metadata_turn() {
                        Ok((channels, more)) => {
                            if recovering {
                                if more {
                                    // An EOF/tail-only continuation has not yet
                                    // reexamined the failed row in its next cut.
                                    metadata_retry_at = Some(tokio::time::Instant::now());
                                } else {
                                    self.note_ui_change();
                                }
                            }
                            result.more |= more;
                            for channel in channels {
                                let members = self.channel_members(channel);
                                self.distribute_channel(channel, &members).await;
                            }
                        }
                        Err(error) => {
                            log::warn!("channel metadata recovery deferred: {error}");
                            #[cfg(test)]
                            self.trace_account_client(&self.identity.public(), || {
                                "channel_recovery_metadata_failed".into()
                            });
                            metadata_retry_at =
                                Some(tokio::time::Instant::now() + CHANNEL_RECOVERY_INTERVAL);
                        }
                    }
                }
                #[cfg(test)]
                self.trace_account_client(&self.identity.public(), || {
                    "channel_recovery_group_metadata_done".into()
                });
                if membership_needed
                    && membership_retry_at.is_none_or(|at| tokio::time::Instant::now() >= at)
                {
                    membership_needed = false;
                    membership_retry_at = None;
                    for (name, outcome) in [
                        ("invitation", self.auto_enroll_one_channel_sibling().await),
                        ("acceptance", self.auto_accept_one_linked_invite().await),
                    ] {
                        match outcome {
                            Ok(changed) => {
                                result.more |= changed;
                                membership_needed |= changed;
                            }
                            Err(error) => {
                                log::warn!("linked channel {name} failed; will retry: {error}");
                                membership_retry_at =
                                    Some(tokio::time::Instant::now() + CHANNEL_RECOVERY_INTERVAL);
                                membership_needed = true;
                            }
                        }
                    }
                    #[cfg(test)]
                    self.trace_account_client(&self.identity.public(), || {
                        "channel_recovery_membership_entered".into()
                    });
                }
                #[cfg(test)]
                self.trace_account_client(&self.identity.public(), || {
                    "channel_recovery_membership_done".into()
                });
                let targets = {
                    let mut pending = self
                        .channel_recovery_pending
                        .lock()
                        .expect("recovery queue not poisoned");
                    let targets = take_channel_recovery_targets(&mut pending, &mut last_target);
                    if !pending.is_empty() {
                        self.channel_recovery_notify.notify_one();
                    }
                    targets
                };
                let active = discovered || !targets.is_empty();
                if active {
                    backoff.reset();
                }
                #[cfg(test)]
                self.trace_account_client(&self.identity.public(), || {
                    format!("channel_recovery_targets_begin count={}", targets.len())
                });
                let failed = std::sync::atomic::AtomicBool::new(false);
                let failed_ref = &failed;
                stream::iter(targets)
                    .for_each_concurrent(
                        CHANNEL_RECOVERY_GROUP_CONCURRENCY,
                        |(channel, cycle)| async move {
                            if node.pull_channel_from_members(channel, cycle, false).await {
                                failed_ref.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        },
                    )
                    .await;
                #[cfg(test)]
                self.trace_account_client(&self.identity.public(), || {
                    "channel_recovery_targets_done".into()
                });
                // Notification-only tests exclude discovery-triggered repair as well.
                if (self.channel_recovery_periodic_enabled(periodic)
                    && (discovered || tokio::time::Instant::now() >= fallback_at))
                    || (fallback_retry && tokio::time::Instant::now() >= fallback_at)
                {
                    #[cfg(test)]
                    self.trace_account_client(&self.identity.public(), || {
                        "channel_recovery_fallback_begin".into()
                    });

                    if discovered && fallback_remaining != 0 {
                        fallback_resweep = true;
                    }
                    if fallback_remaining == 0 {
                        fallback_remaining = self.joined_channel_count();
                    }
                    let batch = fallback_remaining.min(CHANNEL_RECOVERY_WAKE_BATCH);
                    fallback_remaining -= batch;
                    let start = cursor;
                    cursor = cursor.wrapping_add(batch);
                    stream::iter(start..start.saturating_add(batch))
                        .for_each_concurrent(
                            CHANNEL_RECOVERY_GROUP_CONCURRENCY,
                            |index| async move {
                                if node.pull_one_channel_from_member(index).await {
                                    failed_ref.store(true, std::sync::atomic::Ordering::Relaxed);
                                }
                            },
                        )
                        .await;
                    // Independent bounded jitter prevents synchronized idle repair bursts.
                    let jitter =
                        (now_millis() ^ u64::from(self.identity.public().ed25519_pub[0])) as u8;
                    fallback_failed |= failed.load(std::sync::atomic::Ordering::Relaxed);
                    let delay = if fallback_remaining != 0 || std::mem::take(&mut fallback_resweep)
                    {
                        result.more = true;
                        Duration::ZERO
                    } else if fallback_failed {
                        fallback_failed = false;
                        let delay = backoff.next_delay(jitter);
                        failure_delay = Some(delay);
                        delay
                    } else {
                        fallback_retry = false;
                        CHANNEL_RECOVERY_QUIET_INTERVAL
                    };
                    fallback_at = tokio::time::Instant::now() + delay;
                }
                #[cfg(test)]
                self.trace_account_client(&self.identity.public(), || {
                    "channel_recovery_fallback_done".into()
                });
                if failure_delay.is_none()
                    && fallback_remaining == 0
                    && failed.load(std::sync::atomic::Ordering::Relaxed)
                {
                    failure_delay = Some(backoff.next_delay(0));
                }
                if let Some(delay) = failure_delay {
                    // Only failed attempted work owns an early repair deadline.
                    // Successful target invalidations do not rearm quiet scans.
                    fallback_retry = true;
                    fallback_at = fallback_at.min(tokio::time::Instant::now() + delay);
                }
                result.merge(LocalRecoveryWork {
                    more: false,
                    retry_after: failure_delay,
                });
                for deadline in [history_retry_at, metadata_retry_at, membership_retry_at]
                    .into_iter()
                    .flatten()
                {
                    result.merge(LocalRecoveryWork {
                        more: false,
                        retry_after: Some(
                            deadline.saturating_duration_since(tokio::time::Instant::now()),
                        ),
                    });
                }
                result
            };
            tokio::pin!(remote);
            work.merge(self.complete_channel_recovery_turn(remote.as_mut()).await);
            local_continuation = work.more;
            local_at = if work.more {
                Some(tokio::time::Instant::now())
            } else {
                work.retry_after
                    .map(|delay| tokio::time::Instant::now() + delay)
            };
            // Notify can remain immediately ready after bounded local work.
            // Yield so the other owned recovery futures can always be polled.
            tokio::task::yield_now().await;
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
        self.await_channel_metadata(channel).await?;
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
        self.await_channel_metadata(channel).await?;
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
        self.distribute_my_sender_key(channel, &members)?;
        // Key publication advances the signed source too. Complete that local
        // intent before exposing a newly created channel to immediate sends.
        self.await_channel_metadata(channel).await?;
        // This grants automatic account migration permission. Grant it only
        // after creation's final source read, so maintenance cannot append an
        // authority upgrade between sender publication and metadata completion.
        self.record_channel_owner_origin(channel)?;
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
        self.commit_my_sender_key(channel, members)?;
        self.process_channel(channel);
        Ok(())
    }

    /// Commit the same sender WAL without spending another canonical allowance.
    /// Bounded maintenance prepares its accepted tail with its existing budget.
    pub(in crate::node) fn commit_my_sender_key(
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
        self.await_channel_metadata(channel).await?;
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
        let mut owner_control = None;
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
                    super::channel_account_controls::AccountChannelMutation::CommittedControl {
                        event,
                        recipients,
                    } => {
                        owner_control = Some(event);
                        distribution = Some(recipients);
                        return Ok(());
                    }
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
        if let Some(event) = owner_control {
            self.await_account_owner_control(channel, event).await?;
        }
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
        self.await_channel_metadata(channel).await?;
        // Policy serialization precedes host authorization and membership locks.
        // Never retain it during network distribution.
        let policy = self.privacy.gate.read().await;
        let mut distribution = None;
        let mut owner_control = None;
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
                    super::channel_account_controls::AccountChannelMutation::CommittedControl {
                        event,
                        recipients,
                    } => {
                        owner_control = Some(event);
                        distribution = Some(recipients);
                        return Ok(());
                    }
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
        if let Some(event) = owner_control {
            self.await_account_owner_control(channel, event).await?;
        }
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
        self.await_channel_metadata(channel).await?;
        // Policy serialization precedes host authorization and membership locks.
        // Never retain it during network distribution.
        let policy = self.privacy.gate.read().await;
        let mut distribution = None;
        let mut owner_control = None;
        authorize(&mut || {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            match self.try_account_channel_remove(channel, account_id)? {
                super::channel_account_controls::AccountChannelMutation::CommittedControl {
                    event,
                    recipients,
                } => {
                    owner_control = Some(event);
                    distribution = Some(recipients);
                    return Ok(());
                }
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
        if let Some(event) = owner_control {
            self.await_account_owner_control(channel, event).await?;
        }
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
        self.await_channel_metadata(channel).await?;
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
        self.await_channel_metadata(channel).await?;
        let mut distribution = None;
        let mut owner_control = None;
        let mut transfer = None;
        authorize(&mut || {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            match self.try_account_channel_transfer(channel, new_owner)? {
                super::channel_account_controls::AccountChannelMutation::CommittedControl {
                    event,
                    recipients,
                } => {
                    owner_control = Some(event);
                    distribution = Some(recipients);
                    return Ok(());
                }
                super::channel_account_controls::AccountChannelMutation::Changed(members) => {
                    distribution = Some(members);
                    return Ok(());
                }
                super::channel_account_controls::AccountChannelMutation::Unchanged => return Ok(()),
                super::channel_account_controls::AccountChannelMutation::Legacy => {}
            }
            let (owner, accepted, members) = {
                let book = self.channels.lock().expect("channels mutex not poisoned");
                let state = book
                    .state(&channel)
                    .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
                (
                    state.owner().to_string(),
                    state.is_accepted_member(new_owner),
                    state.members().to_vec(),
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
            let (_, event) = self.append_event_with_profile_parent(
                channel,
                EventKind::ChannelOwnerTransfer,
                new_owner.as_bytes().to_vec(),
                None,
            )?;
            transfer = Some(event);
            distribution = Some(members);
            Ok(())
        })?;
        if let Some(event) = owner_control {
            self.await_account_owner_control(channel, event).await?;
        }
        let Some(members) = distribution else {
            return Ok(());
        };
        self.await_channel_metadata(channel).await?;
        if let Some(event) = transfer {
            let projection = self.account_group_projection(channel)?;
            if projection.ownership != Some(event)
                || !projection
                    .state
                    .as_ref()
                    .is_some_and(|state| state.owner() == new_owner)
            {
                return Err(NodeError::Channel(
                    "channel membership changed before ownership transfer completed".into(),
                ));
            }
        }
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
        self.await_channel_metadata(channel).await?;
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
        let Some((members, event)) = distribution else {
            return Ok(());
        };
        self.await_channel_metadata(channel).await?;
        if !self
            .account_group_projection(channel)?
            .is_authorized(event)?
        {
            return Err(NodeError::Channel(
                "legacy departure was not admitted".into(),
            ));
        }
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
        self.await_channel_metadata(channel).await?;
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
                let (members, event) = self.apply_account_self_control(
                    channel,
                    account_self_control::AccountSelfControlInput::Accept,
                    &own_proof,
                )?;
                distribution = Some((members, Some(event)));
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
            distribution = Some((members, None));
            Ok(())
        })?;
        let Some((members, event)) = distribution else {
            return Ok(());
        };
        self.await_channel_metadata(channel).await?;
        if let Some(event) = event
            && !self
                .account_group_projection(channel)?
                .is_authorized(event)?
        {
            return Err(NodeError::Channel(
                "account self control was not admitted".into(),
            ));
        }
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
        #[cfg(test)]
        self.trace_account_client(&self.identity.public(), || {
            format!("channel_control_retry_selected count={len}")
        });
        if len == 0 {
            return;
        }
        let offset = (now_millis() / 10_000) as usize % len;
        for (channel, members) in pending.into_iter().cycle().skip(offset).take(len.min(32)) {
            self.distribute_channel(channel, &members).await;
            #[cfg(test)]
            self.trace_account_client(&self.identity.public(), || {
                format!("channel_control_retry_distributed {channel:?}")
            });
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
        self.await_channel_metadata(channel).await?;
        let id = self.enqueue_channel_message_reply_expiring_if(
            channel,
            text,
            reply_to,
            expires_at_ms,
            &mut authorize,
        )?;
        self.flush_accepted_message(&MessageDestination::Channel(channel), id)
            .await;
        if let Err(error) = self.await_channel_metadata(channel).await {
            log::debug!("accepted channel metadata preparation deferred: {error}");
        }
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
        self.await_channel_metadata(channel).await?;
        let id = self.enqueue_sticker_channel_expiring_if(
            channel,
            sticker_id,
            fallback,
            expires_at_ms,
            &mut authorize,
        )?;
        self.flush_accepted_message(&MessageDestination::Channel(channel), id)
            .await;
        if let Err(error) = self.await_channel_metadata(channel).await {
            log::debug!("accepted channel metadata preparation deferred: {error}");
        }
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
    #[track_caller]
    pub(in crate::node) fn distribute_channel<'a>(
        &'a self,
        channel: ConversationId,
        members: &'a [PublicIdentity],
    ) -> impl std::future::Future<Output = ()> + Send + 'a {
        let origin = std::panic::Location::caller();
        async move {
            self.observe_distribution_origin(origin, false);
            if let Err(error) = self.await_channel_metadata(channel).await {
                log::warn!("accepted channel control preparation deferred: {error}");
                return;
            }
            self.distribute_channel_targets(channel, members, None, true, None)
                .await;
        }
    }

    /// Full control distribution with one exact-message custody probe on each
    /// existing sync exchange. Account operations retain all-member fanout.
    #[track_caller]
    pub(in crate::node) fn distribute_channel_confirming<'a>(
        &'a self,
        channel: ConversationId,
        members: &'a [PublicIdentity],
        event: Option<EventId>,
    ) -> impl std::future::Future<Output = bool> + Send + 'a {
        let origin = std::panic::Location::caller();
        async move {
            self.observe_distribution_origin(origin, true);
            if let Err(error) = self.await_channel_metadata(channel).await {
                log::warn!("accepted channel control preparation deferred: {error}");
                return false;
            }
            self.distribute_channel_targets(channel, members, None, true, event)
                .await
        }
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
        self.channel_custody_count_with_verifier(
            channel,
            id,
            &mut CustodyCertificateVerifier::default(),
        )
    }

    fn channel_custody_count_with_verifier(
        &self,
        channel: ConversationId,
        id: EventId,
        verifier: &mut CustodyCertificateVerifier,
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
        let (principals, unreadable) =
            self.confirmed_channel_custody_principals_with_verifier(channel, id, verifier)?;
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
        self.confirmed_channel_custody_principals_with_verifier(
            channel,
            id,
            &mut CustodyCertificateVerifier::default(),
        )
    }

    fn confirmed_channel_custody_principals_with_verifier(
        &self,
        channel: ConversationId,
        id: EventId,
        verifier: &mut CustodyCertificateVerifier,
    ) -> Result<(HashSet<String>, bool), NodeError> {
        let (confirmed, observations) = self.effective_channel_custody_observations(channel, id)?;
        let members = self
            .channels
            .lock()
            .expect("channels mutex")
            .state(&channel)
            .map(|state| state.members().to_vec())
            .unwrap_or_default();
        // One decision reads every positive witness once. This request-local
        // value never bypasses the next observation's durable readability check.
        let bindings: HashMap<_, _> = observations
            .iter()
            .filter(|meta| !meta.tombstone)
            .filter_map(|meta| {
                let super::account_documents::AccountDocumentKey::ChannelCustody { holder, .. } =
                    &meta.key
                else {
                    return None;
                };
                Some((
                    holder.as_str(),
                    self.channel_custody_holder_binding_with_verifier(&meta.key, verifier),
                ))
            })
            .collect();
        let unreadable = bindings.values().any(Option::is_none);
        let principals = confirmed
            .into_iter()
            .filter_map(|device| {
                if device.starts_with("relay:") {
                    return None;
                }
                if let Some(binding) = bindings.get(device.as_str()) {
                    let (holder, account) = binding.as_ref()?;
                    return self.channel_custody_member_principal(channel, holder, account.clone());
                }
                let member = members.iter().find(|member| member.user_id() == device)?;
                self.channel_custody_member_principal(channel, member, None)
            })
            .collect();
        Ok((principals, unreadable))
    }

    /// Observe at most 256 message rows in input order, preserving unknown counts.
    pub fn channel_custody_counts(
        &self,
        channel: ConversationId,
        ids: &[EventId],
    ) -> Result<ChannelCustodyCounts, NodeError> {
        if ids.len() > 256 {
            return Err(NodeError::InvalidInput(
                "at most 256 message ids are allowed".into(),
            ));
        }
        let mut counts = Vec::with_capacity(ids.len());
        let mut verifier = CustodyCertificateVerifier::default();
        for id in ids {
            #[cfg(test)]
            self.trace_account_client(&self.identity.public(), || {
                "custody_batch_before_row".into()
            });
            let members = self.channel_custody_count_with_verifier(channel, *id, &mut verifier)?;
            let post_offices = if members.is_some() {
                self.channel_post_office_custody_count_with_verifier(channel, *id, &mut verifier)?
            } else {
                None
            };
            counts.push((members, post_offices));
        }
        Ok(counts)
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
        self.channel_post_office_custody_count_with_verifier(
            channel,
            id,
            &mut CustodyCertificateVerifier::default(),
        )
    }

    fn channel_post_office_custody_count_with_verifier(
        &self,
        channel: ConversationId,
        id: EventId,
        verifier: &mut CustodyCertificateVerifier,
    ) -> Result<Option<usize>, NodeError> {
        let (confirmed, observations) = self.effective_channel_custody_observations(channel, id)?;
        let unreadable = observations.iter().any(|meta| !meta.tombstone && matches!(&meta.key, super::account_documents::AccountDocumentKey::ChannelCustody { holder, .. } if holder.starts_with("relay:")) && self.channel_custody_holder_binding_with_verifier(&meta.key, verifier).is_none());
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
                        let changed = if witnessed {
                            let mut store =
                                self.channel_custody.lock().expect("channel custody mutex");
                            let before = store.revision();
                            store.confirm(event_id, &device).is_ok() && store.revision() != before
                        } else {
                            false
                        };
                        if changed {
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
        let changed = {
            let mut store = self.channel_custody.lock().expect("channel custody mutex");
            let before = store.revision();
            let recorded = if held {
                store.confirm(event_id, &peer.public.user_id())
            } else {
                store.revoke(event_id, &peer.public.user_id())
            };
            recorded.is_ok() && store.revision() != before
        };
        if changed {
            self.delivery_status_notify.notify_waiters();
        }
    }

    /// Retry one insufficiently replicated own message per tick. The event log is
    /// the durable pending queue, so a restart cannot forget local-only messages.
    /// Alternate competing replication and missing-report work across message
    /// scan cycles. Return at most one read-only query instead of a replication
    /// exchange, preserving the existing recovery slot and peer rotation.
    #[cfg(test)]
    pub(in crate::node) async fn retry_one_channel_custody(
        &self,
        cursor: usize,
        after: &mut HashMap<ConversationId, (u64, EventId)>,
        cycles: &mut HashMap<ConversationId, usize>,
    ) -> Option<(ConversationId, EventId, PeerRecord)> {
        // Existing real-protocol controls own the legacy position/cycle maps.
        // Adapt only this test-facing call; production retains typed cursors.
        let mut cursors: HashMap<_, _> = after
            .iter()
            .map(|(channel, key)| {
                (
                    *channel,
                    CustodyRetryCursor {
                        after: Some(*key),
                        cycle: cycles.get(channel).copied().unwrap_or(0),
                        ..Default::default()
                    },
                )
            })
            .collect();
        for (channel, cycle) in cycles.iter() {
            cursors
                .entry(*channel)
                .or_insert_with(|| CustodyRetryCursor {
                    cycle: *cycle,
                    ..Default::default()
                });
        }
        let report = self
            .retry_channel_custody_turn(
                cursor,
                &mut cursors,
                1,
                &mut CustodyRetryProgress::default(),
            )
            .await;
        for (channel, state) in cursors {
            if let Some(key) = state.after {
                after.insert(channel, key);
                cycles.insert(channel, state.cycle);
            }
        }
        report
    }

    async fn retry_channel_custody_turn(
        &self,
        cursor: usize,
        cursors: &mut HashMap<ConversationId, CustodyRetryCursor>,
        generation: u64,
        progress: &mut CustodyRetryProgress,
    ) -> Option<(ConversationId, EventId, PeerRecord)> {
        let mut selected_channel = None;
        let mut quiet_turn = false;
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
                progress.quiet_complete = true;
                return Ok(None);
            }
            channels.sort_unstable_by_key(|(channel, _)| *channel.as_bytes());
            let (channel, _) = &channels[cursor % channels.len()];
            let channel = *channel;
            selected_channel = Some(channel);
            let state = cursors.entry(channel).or_default();
            if state.generation != generation {
                state.generation = generation;
                state.restart_sweep();
            }
            if state.complete {
                quiet_turn = true;
                progress.quiet_complete = channels.iter().all(|(id, _)| {
                    cursors
                        .get(id)
                        .is_some_and(|state| state.generation == generation && state.complete)
                });
                return Ok(None);
            }
            let members = self.channel_custody_repair_members(channel);
            let author =
                crate::eventlog::event::Author::from_ed25519(self.identity.public().ed25519_pub);
            let candidates: Vec<_> = {
                let log = self.log.lock().expect("log mutex not poisoned");
                log.message_keys_after(channel, author, state.after, CHANNEL_CUSTODY_SCAN_PER_TICK)?
            };
            let short_page = candidates.len() < CHANNEL_CUSTODY_SCAN_PER_TICK;
            let mut covered = false;
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
            for key in candidates {
                // Advance fair scheduling before any attempt, while retaining
                // the generation's original boundary across quiet pages.
                let (cycle, reached) = state.advance(key);
                covered |= reached;
                let event_id = key.1;
                let (report_first, report_cycle) = custody_retry_order(cycle);
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
                let competing_report = missing_report
                    .as_ref()
                    .is_some_and(|(_, report_event, _)| *report_event == event_id);
                let peer_cycle = if competing_report {
                    report_cycle
                } else {
                    cycle
                };
                if let Some(peer) = select_custody_peer(peers, peer_cycle) {
                    if competing_report && report_first {
                        return Ok(select_custody_peer(report_peers.clone(), report_cycle)
                            .map(|peer| (channel, event_id, peer)));
                    }
                    let _ = tokio::time::timeout(
                        CHANNEL_RECOVERY_PEER_TIMEOUT,
                        self.sync_and_confirm_custody(channel, event_id, &peer),
                    )
                    .await;
                    return Ok(None);
                }
            }
            quiet_turn = missing_report.is_none();
            state.complete = quiet_turn && (short_page || covered);
            progress.quiet_complete = channels.iter().all(|(id, _)| {
                cursors
                    .get(id)
                    .is_some_and(|state| state.generation == generation && state.complete)
            });
            Ok(missing_report)
        }
        .await;
        if !quiet_turn
            && let Some(channel) = selected_channel
            && let Some(state) = cursors.get_mut(&channel)
        {
            // A selected exchange/report or failed classification does not
            // certify a quiet pass. Resume fairly from its committed position.
            state.restart_sweep();
            progress.quiet_complete = false;
        }
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
        self.distribute_channel_targets_outcome(
            channel,
            members,
            fanout,
            replicate_to_post_office,
            confirming,
        )
        .await
        .deferred
    }

    async fn distribute_channel_targets_outcome(
        &self,
        channel: ConversationId,
        members: &[PublicIdentity],
        fanout: Option<usize>,
        replicate_to_post_office: bool,
        confirming: Option<EventId>,
    ) -> ChannelDistributionOutcome {
        let failed = std::sync::atomic::AtomicBool::new(false);
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
                    let failed = &failed;
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
                                let round = request_round(&mut ch, &store, channel).await;
                                #[cfg(test)]
                                self.trace_account_client(&peer.public, || {
                                    match &round {
                                        Ok(_) => "control_fanout_round_ok",
                                        Err(super::session::SessionError::Store(
                                            LogError::StaleCursor,
                                        )) => "control_fanout_round_stale",
                                        Err(error)
                                            if super::session_pool::retryable_sync_error(error) =>
                                        {
                                            "control_fanout_round_failed kind=transport_closed"
                                        }
                                        Err(super::session::SessionError::Transport(_)) => {
                                            "control_fanout_round_failed kind=transport"
                                        }
                                        Err(super::session::SessionError::Store(_)) => {
                                            "control_fanout_round_failed kind=store"
                                        }
                                        Err(super::session::SessionError::Serialization(_)) => {
                                            "control_fanout_round_failed kind=serialization"
                                        }
                                        Err(super::session::SessionError::UnexpectedMessage) => {
                                            "control_fanout_round_failed kind=unexpected_message"
                                        }
                                        Err(super::session::SessionError::NoProgress) => {
                                            "control_fanout_round_failed kind=no_progress"
                                        }
                                        Err(super::session::SessionError::ResourceUnavailable) => {
                                            "control_fanout_round_failed kind=resource_unavailable"
                                        }
                                    }
                                    .into()
                                });
                                if round.as_ref().is_err_and(|error| {
                                    !matches!(
                                        error,
                                        super::session::SessionError::Transport(
                                            crate::transport::TransportError::AdmissionDenied
                                        )
                                    )
                                }) {
                                    failed.store(true, std::sync::atomic::Ordering::Relaxed);
                                }
                                if round.is_ok() {
                                    self.recycle_sync_channel(ch);
                                } else if round.as_ref().is_err_and(|error| {
                                    matches!(
                                        error,
                                        super::session::SessionError::Store(LogError::StaleCursor)
                                    ) || super::session_pool::retryable_sync_error(error)
                                }) {
                                    // A changed local view or closed remote session did
                                    // not finish this round. Keep the journal ticket
                                    // for a fresh view through the existing backoff.
                                    deferred.store(true, std::sync::atomic::Ordering::Relaxed);
                                }
                                Ok(())
                            }
                        })
                        .await;
                        if !matches!(
                            &result,
                            Ok(Ok(())) | Ok(Err(crate::transport::TransportError::AdmissionDenied))
                        ) {
                            failed.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
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
                    .replicate_to_post_office_targets_observing(
                        channel,
                        members
                            .iter()
                            .filter(|member| member.user_id() != me)
                            .cloned()
                            .collect(),
                        confirming,
                        Some(&failed),
                    )
                    .await;
                if result.is_err() {
                    failed.store(true, std::sync::atomic::Ordering::Relaxed);
                }
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
            Ok(deferred) => ChannelDistributionOutcome {
                deferred,
                failed: failed.load(std::sync::atomic::Ordering::Relaxed),
            },
            Err(error) => {
                log::warn!("channel distribution deferred: {error}");
                ChannelDistributionOutcome {
                    deferred: true,
                    failed: true,
                }
            }
        }
    }

    /// Reconcile one joined channel with an online member. A rotating cursor gives
    /// every channel and member a turn without making the background loop dial the
    /// full group. Any member holding an event can serve it, even if its author is
    /// offline. The ordinary sync store enforces membership and validates events.
    pub(in crate::node) async fn pull_one_channel_from_member(
        self: &Arc<Self>,
        cursor: usize,
    ) -> bool {
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
            return self
                .pull_channel_from_members(*channel, cursor / channels.len(), true)
                .await;
        }
        false
    }

    async fn pull_channel_from_members(
        self: &Arc<Self>,
        channel: ConversationId,
        cursor: usize,
        reconcile_controls: bool,
    ) -> bool {
        let channel_id = &channel;
        let me = self.user_id();
        let members = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let Some(state) = book.state(channel_id) else {
                return false;
            };
            if !state.is_member(&me) || state.is_dissolved() {
                return false;
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
        #[cfg(test)]
        self.trace_account_client(&self.identity.public(), || {
            format!("channel_recovery_local_begin channel={channel:?}")
        });
        // Local projection repair is independent of a reachable remote holder.
        let local_failed = if let Err(error) = self.process_channel_recovery_local(*channel_id) {
            log::warn!("channel local recovery deferred: {error}");
            true
        } else {
            false
        };
        #[cfg(test)]
        self.trace_account_client(&self.identity.public(), || {
            format!("channel_recovery_local_done channel={channel:?}")
        });
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
            return local_failed;
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
        let rounds = (0..peers.len().min(CHANNEL_RECOVERY_PEERS_PER_TICK)).map(|offset| {
            let peer = peers[(start + offset) % peers.len()].clone();
            let node = self.clone();
            async move {
                #[cfg(test)]
                node.trace_account_client(&peer.public, || {
                    format!("channel_recovery_peer_begin channel={channel:?}")
                });
                let result = tokio::time::timeout(CHANNEL_RECOVERY_PEER_TIMEOUT, async {
                    let Ok(mut conn) = node.privacy_sync_dial(peer.addr, &peer.public).await else {
                        node.observe_mailbox_work(
                            super::work_observation::reason::CHANNEL_SYNC_TRANSPORT_FAILED,
                            None,
                        );
                        return None;
                    };
                    #[cfg(test)]
                    node.trace_account_client(&peer.public, || {
                        format!("channel_recovery_connection_ready channel={channel:?}")
                    });
                    // Recovery fetches retained history; it must not turn an
                    // outbound connection into a sharing-preference bypass.
                    let sharing = match node.shares_replica(&channel) {
                        Ok(sharing) => sharing,
                        Err(error) => {
                            node.observe_mailbox_work(
                                super::work_observation::reason::CHANNEL_SYNC_STORE_FAILED,
                                None,
                            );
                            log::warn!("channel replica export deferred: {error}");
                            return None;
                        }
                    };
                    let store = if sharing {
                        node.sync_store(conn.peer_identity())
                    } else {
                        node.pull_sync_store(conn.peer_identity())
                    };
                    #[cfg(test)]
                    node.trace_account_client(&peer.public, || {
                        format!("channel_recovery_sync_begin channel={channel:?}")
                    });
                    let round = request_round(&mut conn, &store, channel).await;
                    let reason = match &round {
                        Ok(report) if report.applied > 0 => {
                            super::work_observation::reason::CHANNEL_SYNC_OK_APPLIED
                        }
                        Ok(_) => super::work_observation::reason::CHANNEL_SYNC_OK_ZERO,
                        Err(super::session::SessionError::NoProgress) => {
                            super::work_observation::reason::CHANNEL_SYNC_NO_PROGRESS
                        }
                        Err(super::session::SessionError::Transport(_)) => {
                            super::work_observation::reason::CHANNEL_SYNC_TRANSPORT_FAILED
                        }
                        Err(super::session::SessionError::Store(_)) => {
                            super::work_observation::reason::CHANNEL_SYNC_STORE_FAILED
                        }
                        Err(
                            super::session::SessionError::Serialization(_)
                            | super::session::SessionError::UnexpectedMessage
                            | super::session::SessionError::ResourceUnavailable,
                        ) => super::work_observation::reason::CHANNEL_SYNC_PROTOCOL_FAILED,
                    };
                    node.observe_mailbox_work(reason, None);
                    let report = match round {
                        Ok(report) => report,
                        Err(error) => {
                            log::warn!("channel recovery exchange deferred: {error}");
                            return None;
                        }
                    };
                    #[cfg(test)]
                    node.trace_account_client(&peer.public, || {
                        format!("channel_recovery_sync_done channel={channel:?}")
                    });
                    #[cfg(test)]
                    node.trace_account_client(&peer.public, || {
                        format!(
                            "channel_recovery_report applied={} rejected={}",
                            report.applied,
                            report.rejected.len()
                        )
                    });
                    node.recycle_sync_channel(conn);
                    Some((report.applied, report.rejected.is_empty()))
                })
                .await;
                if result.is_err() {
                    node.observe_mailbox_work(
                        super::work_observation::reason::CHANNEL_SYNC_TIMEOUT,
                        None,
                    );
                }
                #[cfg(test)]
                node.trace_account_client(&peer.public, || {
                    let outcome = match &result {
                        Ok(Some(_)) => "complete",
                        Ok(None) => "failed",
                        Err(_) => "timeout",
                    };
                    format!("channel_recovery_peer_end channel={channel:?} outcome={outcome}")
                });
                if let Ok(Some((applied, clean))) = result {
                    #[cfg(test)]
                    node.trace_account_client(&peer.public, || {
                        "channel_recovery_reception_begin".into()
                    });
                    node.process_channel_reception(channel, "catch_up");
                    #[cfg(test)]
                    node.trace_account_client(&peer.public, || {
                        "channel_recovery_channel_reception_end".into()
                    });
                    node.process_file_events_reception(channel, "catch_up");
                    #[cfg(test)]
                    node.trace_account_client(&peer.public, || {
                        "channel_recovery_reception_end".into()
                    });
                    Some((applied, clean))
                } else {
                    None
                }
            }
        });
        let (checked, clean, applied_progress) = complete_channel_peer_rounds(self, rounds).await;
        if applied_progress {
            self.schedule_channel_recovery(channel);
        }
        #[cfg(test)]
        self.trace_account_client(&self.identity.public(), || {
            format!("channel_recovery_group_progress applied={applied_progress}")
        });
        #[cfg(test)]
        self.trace_account_client(&self.identity.public(), || {
            format!(
                "channel_recovery_group_done channel={channel:?} checked={checked} clean={clean}"
            )
        });
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
        local_failed || !checked || !clean
    }
}

#[cfg(test)]
mod fanout_tests {
    use super::*;
    use crate::discovery::Announce;
    use crate::identity::device::DeviceIdentity;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::Instant;

    async fn custody_notification_probe_fixture(sync: bool) {
        use crate::{
            identity::account::Account,
            node::test_support::{IndependentPeerRuntime, node_with_files},
        };
        use futures_util::FutureExt;
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let remote_account = Account::generate();
        let identity = DeviceIdentity::generate();
        let remote_identity = DeviceIdentity::generate();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let proof =
            Announce::new_with_account(&remote_identity, &remote_account, "holder", address.port());
        let (source, _, _) = node_with_files(&dir.path().join("source"), identity, account, &proof);
        let own = Announce::new_with_account(&source.identity, &source.account, "source", 1);
        let (holder, _, _) = node_with_files(
            &dir.path().join("holder"),
            remote_identity,
            remote_account,
            &own,
        );
        let mut server = IndependentPeerRuntime::start_multithreaded_with(
            holder.clone(),
            listener,
            |node, listener| node.run_inbound_connections_for_test(listener),
        )
        .await;
        server.launch();
        let outcome = std::panic::AssertUnwindSafe(async {
            let channel = source
                .create_channel("custody notification", vec![holder.identity.public()])
                .await
                .unwrap();
            let event = source
                .enqueue_channel_message_reply_expiring_if(
                    channel,
                    b"held probe",
                    None,
                    None,
                    |commit| commit(),
                )
                .unwrap();
            let peer = source.routing_peer(&holder.user_id()).unwrap();
            source
                .sync_and_confirm_custody_admitted(channel, event, &peer)
                .await
                .unwrap();
            holder.process_channel(channel);
            source
                .sync_and_confirm_custody_admitted(channel, event, &peer)
                .await
                .unwrap();
            assert!(holder.log.lock().unwrap().get(&event).unwrap().is_some());
            // Leave the published positive witness unchanged. The first probe
            // must still notify when it repairs only the local custody fact.
            source
                .channel_custody
                .lock()
                .unwrap()
                .revoke(event, &holder.user_id())
                .unwrap();
            let first = source.delivery_status_notify.notified();
            tokio::pin!(first);
            first.as_mut().enable();
            if sync {
                source
                    .sync_and_confirm_custody_admitted(channel, event, &peer)
                    .await
                    .unwrap();
            } else {
                source
                    .recheck_channel_custody_peer(channel, event, &peer)
                    .await;
            }
            assert!(
                first.as_mut().now_or_never().is_some(),
                "first held fact must notify even with an unchanged document"
            );
            assert!(
                source
                    .channel_custody
                    .lock()
                    .unwrap()
                    .peers(event)
                    .unwrap()
                    .contains(&holder.user_id())
            );
            let repeated = source.delivery_status_notify.notified();
            tokio::pin!(repeated);
            repeated.as_mut().enable();
            if sync {
                source
                    .sync_and_confirm_custody_admitted(channel, event, &peer)
                    .await
                    .unwrap();
            } else {
                source
                    .recheck_channel_custody_peer(channel, event, &peer)
                    .await;
            }
            let unchanged_held_woke = repeated.as_mut().now_or_never().is_some();
            // An authenticated negative for an absent event retracts a real
            // journal positive, then remains quiet on the identical next probe.
            let absent = EventId::new([243; 32]);
            source
                .publish_channel_custody(channel, absent, &peer.public, false, false, false)
                .unwrap();
            source
                .channel_custody
                .lock()
                .unwrap()
                .confirm(absent, &holder.user_id())
                .unwrap();
            let revoked = source.delivery_status_notify.notified();
            tokio::pin!(revoked);
            revoked.as_mut().enable();
            source
                .recheck_channel_custody_peer(channel, absent, &peer)
                .await;
            assert!(
                revoked.as_mut().now_or_never().is_some(),
                "first negative must notify when it retracts custody"
            );
            assert!(
                !source
                    .channel_custody
                    .lock()
                    .unwrap()
                    .peers(absent)
                    .unwrap()
                    .contains(&holder.user_id())
            );
            let negative = source.delivery_status_notify.notified();
            tokio::pin!(negative);
            negative.as_mut().enable();
            source
                .recheck_channel_custody_peer(channel, absent, &peer)
                .await;
            let unchanged_negative_woke = negative.as_mut().now_or_never().is_some();
            let document = source.delivery_status_notify.notified();
            tokio::pin!(document);
            document.as_mut().enable();
            source
                .publish_channel_custody(
                    channel,
                    EventId::new([244; 32]),
                    &peer.public,
                    false,
                    false,
                    false,
                )
                .unwrap();
            assert!(
                document.as_mut().now_or_never().is_some(),
                "a changed shared witness retains its independent notification"
            );
            assert!(
                !unchanged_held_woke,
                "unchanged held probe must not wake delivery status"
            );
            assert!(
                !unchanged_negative_woke,
                "unchanged negative probe must not wake delivery status"
            );
        })
        .catch_unwind()
        .await;
        server.stop().await;
        source.runtime_work.close();
        source.runtime_work.drain().await;
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    struct ControlRetryProbe {
        scans: Arc<std::sync::atomic::AtomicUsize>,
        dispatches: Arc<std::sync::atomic::AtomicUsize>,
        groups: Arc<std::sync::Mutex<HashSet<String>>>,
        per_group: Arc<std::sync::Mutex<HashMap<String, usize>>>,
        local_lanes: Arc<std::sync::Mutex<HashMap<String, usize>>>,
        distributed: tokio::sync::mpsc::UnboundedReceiver<()>,
        completed: tokio::sync::mpsc::UnboundedReceiver<()>,
        selected: tokio::sync::mpsc::UnboundedReceiver<()>,
    }

    fn control_retry_probe(node: &Node) -> ControlRetryProbe {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let scans = Arc::new(AtomicUsize::new(0));
        let dispatches = Arc::new(AtomicUsize::new(0));
        let groups = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let per_group = Arc::new(std::sync::Mutex::new(HashMap::<String, usize>::new()));
        let local_lanes = Arc::new(std::sync::Mutex::new(HashMap::<String, usize>::new()));
        let (distributed, distribution_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (completed, completion_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (selected, receiver) = tokio::sync::mpsc::unbounded_channel();
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let scans = scans.clone();
            let dispatches = dispatches.clone();
            let groups = groups.clone();
            let per_group = per_group.clone();
            let local_lanes = local_lanes.clone();
            move |_, phase| {
                if phase.starts_with("channel_recovery_") && phase.ends_with("_entered") {
                    *local_lanes
                        .lock()
                        .unwrap()
                        .entry(phase.to_owned())
                        .or_default() += 1;
                }
                if phase == "channel_control_retry_complete" {
                    let _ = completed.send(());
                }
                if let Some(channel) = phase.strip_prefix("channel_control_retry_distributed ") {
                    dispatches.fetch_add(1, Ordering::SeqCst);
                    groups.lock().unwrap().insert(channel.to_owned());
                    *per_group
                        .lock()
                        .unwrap()
                        .entry(channel.to_owned())
                        .or_default() += 1;
                    let _ = distributed.send(());
                }
                if phase.starts_with("channel_control_retry_selected count=") {
                    scans.fetch_add(1, Ordering::SeqCst);
                    let _ = selected.send(());
                }
            }
        }));
        ControlRetryProbe {
            scans,
            dispatches,
            groups,
            per_group,
            local_lanes,
            distributed: distribution_receiver,
            completed: completion_receiver,
            selected: receiver,
        }
    }

    fn control_retry_node(path: &std::path::Path) -> Arc<Node> {
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (messages, _) = tokio::sync::mpsc::unbounded_channel();
        let (files, _) = tokio::sync::mpsc::unbounded_channel();
        Node::open(
            DeviceIdentity::generate(),
            Arc::new(std::sync::Mutex::new(
                crate::discovery::roster::Roster::default(),
            )),
            dm,
            messages,
            files,
            &path.join("events.log"),
            &path.join("sent.log"),
            "pw",
        )
        .unwrap()
    }

    async fn stop_control_retry_owner(node: &Arc<Node>, worker: tokio::task::JoinHandle<()>) {
        worker.abort();
        let _ = worker.await;
        node.runtime_work.close();
        node.sessions.clear();
        node.runtime_work.drain().await;
    }

    #[tokio::test]
    async fn accept_owner_held_control_keeps_background_metadata_publication_progressing() {
        use std::sync::atomic::Ordering;
        // Explicit legacy-Core factory: no modern account auto-enrollment can
        // prepare this metadata on behalf of the recovery owner under test.
        let dir = tempfile::tempdir().unwrap();
        let node = control_retry_node(dir.path());
        let peer = DeviceIdentity::generate();
        let channel = node
            .create_channel("held control metadata", vec![peer.public()])
            .await
            .unwrap();
        node.set_channel_replica_enabled(channel, true)
            .await
            .unwrap();
        node.set_channel_announcement_if(channel, Some("signed metadata"), None, |commit| commit())
            .await
            .unwrap();
        let publication = node
            .log
            .lock()
            .unwrap()
            .events(&channel)
            .unwrap()
            .into_iter()
            .find(|event| event.kind == EventKind::ChannelMetadata)
            .unwrap();
        // Finish initial local preparation BEFORE introducing the held remote.
        for _ in 0..128 {
            node.retry_channel_plaintext();
            let _ = node.retry_group_metadata().unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let proof = Announce::new(&peer, "held control peer", address.port());
        let (accepted, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
        let (release, mut held) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut sockets = Vec::new();
            loop {
                tokio::select! {
                    _ = &mut held => break,
                    incoming = listener.accept(), if sockets.len() < 16 => {
                        let (socket, _) = incoming.unwrap();
                        // Hold the actual TCP/Noise handshake owner. No fake
                        // future stands in for the production control exchange.
                        sockets.push(socket);
                        let _ = accepted.send(());
                    }
                }
            }
            drop(sockets);
        });
        let mut probe = control_retry_probe(&node);
        let original_hook = node
            .account_client_test_hook
            .lock()
            .unwrap()
            .clone()
            .unwrap();
        let (local_done, mut local_completions) = tokio::sync::mpsc::unbounded_channel();
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new(move |peer, phase| {
            if phase == "channel_recovery_group_metadata_done" {
                let _ = local_done.send(());
            }
            original_hook(peer, phase);
        }));
        // Keep BOTH actual local/control children under their production parent,
        // but exclude independent account and periodic joined-group networking.
        let roster_changes = node.roster.lock().unwrap().changes();
        let worker = tokio::spawn(node.clone().run_channel_recovery_with_maintenance(
            roster_changes,
            false,
            std::future::pending(),
        ));
        let local_settled =
            tokio::time::timeout(Duration::from_secs(3), local_completions.recv()).await;
        let controls_settled =
            tokio::time::timeout(Duration::from_secs(3), probe.completed.recv()).await;
        let before = probe.dispatches.load(Ordering::SeqCst);
        while probe.selected.try_recv().is_ok() {}
        // Keep the production roster source attached. Notification-only mode
        // prevents an unrelated periodic pull from owning the held endpoint.
        node.roster
            .lock()
            .unwrap()
            .update(&proof, address.ip(), &node.user_id());
        let selected = tokio::time::timeout(Duration::from_secs(3), probe.selected.recv()).await;
        let connected = tokio::time::timeout(Duration::from_secs(3), arrivals.recv()).await;
        // Reuse a valid owner-signed publication envelope with a fresh signed
        // event/ancestry. The mutator does NOT call a metadata display getter,
        // set_channel_announcement, retry_group_metadata, or await_metadata.
        node.append_event(
            channel,
            EventKind::ChannelMetadata,
            publication.ciphertext.clone(),
        )
        .unwrap();
        let pending = node
            .log
            .lock()
            .unwrap()
            .events(&channel)
            .unwrap()
            .into_iter()
            .find(|event| event.kind == EventKind::ChannelMetadata && event.id != publication.id)
            .unwrap();
        node.process_channel(channel); // Canonical reception only; no field index preparation.
        let database = node.channel_checkpoints.profile().database();
        let published = || {
            database
                .read(|c| {
                    Ok(c.query_row(
                        "SELECT EXISTS(SELECT 1 FROM group_metadata_publications WHERE event=?1)",
                        [pending.id.as_bytes().as_slice()],
                        |row| row.get::<_, bool>(0),
                    )?)
                })
                .unwrap()
        };
        let initially_unpublished = !published();
        // Actual retained wake after a real source append. Read-only SQL is the
        // oracle; observing metadata must never prepare it in this fixture.
        node.channel_recovery_notify.notify_one();
        let progress = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if published() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        let control_still_held = probe.dispatches.load(Ordering::SeqCst) == before;
        let server_still_held = !server.is_finished();
        // Preserve the observation BEFORE releasing the real control socket.
        let _ = release.send(());
        server.await.unwrap();
        worker.abort();
        let _ = worker.await;
        // Validation only AFTER preserving the before-release result. This is
        // not the progress oracle: it diagnoses an invalid publication fixture.
        for _ in 0..128 {
            let _ = node.retry_group_metadata().unwrap();
        }
        let valid_after_release = published();
        node.runtime_work.close();
        node.sessions.clear();
        node.runtime_work.drain().await;
        eprintln!(
            "held metadata: local_settled={} controls_settled={} before_release={} after_release_valid={valid_after_release}",
            matches!(local_settled, Ok(Some(()))),
            matches!(controls_settled, Ok(Some(()))),
            progress.is_ok()
        );
        assert!(
            matches!(local_settled, Ok(Some(()))) && matches!(controls_settled, Ok(Some(()))),
            "both real child owners must settle before introducing the held peer"
        );
        assert!(
            valid_after_release,
            "manual post-release validation must accept the actual signed publication fixture"
        );
        assert!(
            matches!(selected, Ok(Some(()))),
            "actual control owner must select its durable replay"
        );
        assert!(
            matches!(connected, Ok(Some(()))),
            "actual pending control must reach a TCP/Noise owner"
        );
        assert!(
            initially_unpublished,
            "foreground source mutation must not satisfy the field-index oracle"
        );
        assert!(
            control_still_held && server_still_held,
            "control network ownership must remain held during local observation"
        );
        assert!(
            progress.is_ok(),
            "background metadata publication must finish before held control release"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn accept_owner_control_retry_is_quiet_before_its_repair_deadline() {
        use std::sync::atomic::Ordering;
        let dir = tempfile::tempdir().unwrap();
        let node = control_retry_node(dir.path());
        let mut probe = control_retry_probe(&node);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let worker = tokio::spawn(
            node.clone()
                .run_accept_loop_without_account_repair(listener),
        );
        let started = tokio::time::timeout(Duration::from_secs(5), probe.selected.recv())
            .await
            .is_ok_and(|value| value.is_some());
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        let before = probe.scans.load(Ordering::SeqCst);
        let source = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .source_revision()
            .unwrap();
        tokio::time::advance(Duration::from_secs(10)).await;
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_secs(10)).await;
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        let after = probe.scans.load(Ordering::SeqCst);
        let after_source = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .source_revision()
            .unwrap();
        stop_control_retry_owner(&node, worker).await;
        eprintln!(
            "actual accept-owner control scans {before}->{after}; source {source}->{after_source}"
        );
        assert!(
            started && before > 0,
            "actual production accept owner completes its initial real control selection"
        );
        assert_eq!(
            source, after_source,
            "no source change qualifies another scan"
        );
        assert_eq!(
            after, before,
            "settled controls must not be rescanned on legacy ten-second ticks before sparse repair"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn accept_owner_pending_control_parks_but_explicit_manual_retry_still_runs() {
        use std::sync::atomic::Ordering;
        let dir = tempfile::tempdir().unwrap();
        let node = control_retry_node(dir.path());
        let channel = node
            .create_account_channel_invited_if("durable replica control", Vec::new(), |commit| {
                commit()
            })
            .await
            .unwrap();
        node.set_channel_replica_enabled(channel, false)
            .await
            .unwrap();
        let durable = node
            .channels
            .lock()
            .unwrap()
            .pending_replica_preferences(&node.user_id())
            .iter()
            .any(|(id, _)| *id == channel);
        let mut probe = control_retry_probe(&node);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let worker = tokio::spawn(
            node.clone()
                .run_accept_loop_without_account_repair(listener),
        );
        let started = tokio::time::timeout(Duration::from_secs(5), probe.selected.recv())
            .await
            .is_ok_and(|value| value.is_some());
        for _ in 0..256 {
            tokio::task::yield_now().await;
        }
        let before = probe.dispatches.load(Ordering::SeqCst);
        tokio::time::advance(Duration::from_secs(10)).await;
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_secs(10)).await;
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        let after = probe.dispatches.load(Ordering::SeqCst);
        while probe.selected.try_recv().is_ok() {}
        while probe.completed.try_recv().is_ok() {}
        let changed_at = tokio::time::Instant::now();
        // This foreground operation distributes too; only the actual owner
        // selector/completion witness can establish retained replay admission.
        node.set_channel_replica_enabled(channel, true)
            .await
            .unwrap();
        let value_selected = tokio::time::timeout(Duration::from_secs(1), probe.selected.recv())
            .await
            .is_ok_and(|v| v.is_some());
        let value_completed = tokio::time::timeout(Duration::from_secs(1), probe.completed.recv())
            .await
            .is_ok_and(|v| v.is_some());
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        let changed = probe.dispatches.load(Ordering::SeqCst);
        let change_elapsed = changed_at.elapsed();
        tokio::time::advance(Duration::from_secs(10)).await;
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        let parked = probe.dispatches.load(Ordering::SeqCst);
        node.retry_channel_controls().await;
        let manual = probe.dispatches.load(Ordering::SeqCst);
        let still_durable = node
            .channels
            .lock()
            .unwrap()
            .pending_replica_preferences(&node.user_id())
            .iter()
            .any(|(id, _)| *id == channel);
        stop_control_retry_owner(&node, worker).await;
        eprintln!("actual pending-control dispatches {before}->{after}; explicit manual {manual}");
        assert!(
            durable && still_durable,
            "a successful pass never clears signed durable replay facts as if they were remote ACKs"
        );
        assert!(
            started && before > 0,
            "valid retained replica preference is actually offered by the accept owner"
        );
        assert!(
            manual > parked,
            "the explicit public retry API remains forceful"
        );
        assert!(
            value_selected
                && value_completed
                && changed > after
                && change_elapsed < CHANNEL_RECOVERY_QUIET_INTERVAL,
            "a real replica preference value change promptly admits an owner replay, without periodic rescue"
        );
        assert_eq!(
            parked, changed,
            "the changed control settles again after its finite replay"
        );
        assert_eq!(
            after, before,
            "unchanged retained control facts do not require continuous background redistribution"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn accept_owner_pending_control_tail_drains_beyond_thirty_two_before_parking() {
        let dir = tempfile::tempdir().unwrap();
        let node = control_retry_node(dir.path());
        for _ in 0..33 {
            let channel = node
                .create_account_channel_invited_if("bounded control tail", Vec::new(), |commit| {
                    commit()
                })
                .await
                .unwrap();
            node.set_channel_replica_enabled(channel, false)
                .await
                .unwrap();
        }
        let mut probe = control_retry_probe(&node);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let worker = tokio::spawn(
            node.clone()
                .run_accept_loop_without_account_repair(listener),
        );
        let started = tokio::time::timeout(Duration::from_secs(5), probe.selected.recv())
            .await
            .is_ok_and(|value| value.is_some());
        // Wait on the actual owner's finite continuation, bounded strictly below
        // the legacy ten-second timer and the new sparse repair deadline.
        let drained = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if probe.groups.lock().unwrap().len() == 33 {
                    break;
                }
                if probe.distributed.recv().await.is_none() {
                    break;
                }
            }
        })
        .await
        .is_ok();
        let actual = probe.groups.lock().unwrap().len();
        stop_control_retry_owner(&node, worker).await;
        eprintln!("actual accept-owner pending-control tail visited {actual}/33");
        assert!(
            started,
            "the real accept owner starts its pending-control cut"
        );
        assert!(
            drained && actual == 33,
            "all retained controls, including the thirty-third, own a prompt finite continuation"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn accept_owner_identical_presence_is_quiet_and_new_route_replays_controls() {
        use std::sync::atomic::Ordering;
        let dir = tempfile::tempdir().unwrap();
        let node = control_retry_node(dir.path());
        let channel = node
            .create_account_channel_invited_if("retained control", Vec::new(), |commit| commit())
            .await
            .unwrap();
        node.set_channel_replica_enabled(channel, false)
            .await
            .unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new(&peer, "route input", 9);
        node.roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.2".parse().unwrap(), &node.user_id());
        let mut probe = control_retry_probe(&node);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let worker = tokio::spawn(
            node.clone()
                .run_accept_loop_without_account_repair(listener),
        );
        let started = tokio::time::timeout(Duration::from_secs(5), probe.completed.recv())
            .await
            .is_ok_and(|v| v.is_some());
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        while probe.completed.try_recv().is_ok() {}
        let before = probe.dispatches.load(Ordering::SeqCst);
        node.roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.2".parse().unwrap(), &node.user_id());
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        let identical = probe.dispatches.load(Ordering::SeqCst);
        node.roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.3".parse().unwrap(), &node.user_id());
        let changed = tokio::time::timeout(Duration::from_secs(1), probe.completed.recv())
            .await
            .is_ok_and(|v| v.is_some());
        let after = probe.dispatches.load(Ordering::SeqCst);
        stop_control_retry_owner(&node, worker).await;
        assert!(started && before > 0);
        assert_eq!(
            identical, before,
            "unchanged signed presence does not earn replay credit"
        );
        assert!(
            changed && after > identical,
            "new route fact promptly replays durable controls"
        );
    }

    #[tokio::test]
    async fn accept_owner_failed_control_retries_without_replaying_healthy_or_local_lanes() {
        use std::sync::atomic::Ordering;
        let dir = tempfile::tempdir().unwrap();
        let node = control_retry_node(dir.path());
        let peer = DeviceIdentity::generate();
        let failed = node
            .create_channel("unreachable control", vec![peer.public()])
            .await
            .unwrap();
        node.set_channel_replica_enabled(failed, false)
            .await
            .unwrap();
        let healthy = node
            .create_account_channel_invited_if("settled control", Vec::new(), |commit| commit())
            .await
            .unwrap();
        node.set_channel_replica_enabled(healthy, false)
            .await
            .unwrap();
        // Keep ownership of the listener: an accepted Noise preamble followed
        // by EOF proves a real failed attempt, unlike an unreserved free port.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server = tokio::spawn({
            let attempts = attempts.clone();
            async move {
                use tokio::io::AsyncReadExt;
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut byte = [0u8; 1];
                    if tokio::time::timeout(Duration::from_secs(1), socket.read(&mut byte))
                        .await
                        .is_ok_and(|read| read.is_ok_and(|count| count > 0))
                    {
                        attempts.fetch_add(1, Ordering::SeqCst);
                    }
                    // Dropping the accepted socket ends the real handshake.
                }
            }
        });
        let proof = Announce::new(&peer, "EOF route", port);
        node.roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.1".parse().unwrap(), &node.user_id());
        let mut probe = control_retry_probe(&node);
        // Keep the canonical control/local parent while excluding unrelated
        // account migration and sparse network repair for this deadline case.
        let worker = tokio::spawn(node.clone().run_channel_recovery_with_maintenance(
            Arc::new(Notify::new()),
            false,
            std::future::pending(),
        ));
        let started = tokio::time::timeout(Duration::from_secs(5), probe.completed.recv())
            .await
            .is_ok_and(|v| v.is_some());
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        while probe.completed.try_recv().is_ok() {}
        let before = probe.per_group.lock().unwrap().clone();
        let local_before = probe.local_lanes.lock().unwrap().clone();
        let scans = probe.scans.load(Ordering::SeqCst);
        let failure_at = tokio::time::Instant::now();
        let attempted = attempts.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(2)).await;
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        let early = probe.scans.load(Ordering::SeqCst);
        tokio::time::sleep_until(failure_at + Duration::from_secs(3)).await;
        let retried = tokio::time::timeout(Duration::from_millis(500), probe.completed.recv())
            .await
            .is_ok_and(|v| v.is_some());
        let after = probe.per_group.lock().unwrap().clone();
        let local_after = probe.local_lanes.lock().unwrap().clone();
        stop_control_retry_owner(&node, worker).await;
        server.abort();
        let _ = server.await;
        eprintln!(
            "control failure startup={started} retry={retried} actual_TCP_attempts={attempted} scans={scans}->{early}"
        );
        assert!(
            attempted > 0,
            "the owned EOF listener observed an actual Noise handshake attempt"
        );
        assert!(
            started && retried,
            "actual failed TCP control attempt retains its three-second retry"
        );
        assert!(
            before.get(&format!("{healthy:?}")).copied().unwrap_or(0) > 0
                && before.get(&format!("{failed:?}")).copied().unwrap_or(0) > 0,
            "both healthy and failed controls were actually selected before retry"
        );
        assert_eq!(early, scans, "no retry before the owned failure deadline");
        assert_eq!(
            before.get(&format!("{healthy:?}")),
            after.get(&format!("{healthy:?}")),
            "a failed group cannot reset successful group replay"
        );
        assert!(
            after.get(&format!("{failed:?}")).copied().unwrap_or(0)
                > before.get(&format!("{failed:?}")).copied().unwrap_or(0),
            "failed signed control is actually offered again"
        );
        assert_eq!(
            local_before, local_after,
            "control retry cannot restart settled history/metadata/membership lanes"
        );
    }

    #[tokio::test]
    async fn accept_owner_same_endpoint_expired_presence_replays_retained_control() {
        use std::sync::atomic::Ordering;
        let dir = tempfile::tempdir().unwrap();
        let node = control_retry_node(dir.path());
        let channel = node
            .create_account_channel_invited_if("retained replay", Vec::new(), |commit| commit())
            .await
            .unwrap();
        node.set_channel_replica_enabled(channel, false)
            .await
            .unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new(&peer, "returning peer", 9);
        let ip = "127.0.0.2".parse().unwrap();
        node.roster
            .lock()
            .unwrap()
            .update(&proof, ip, &node.user_id());
        let mut probe = control_retry_probe(&node);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let worker = tokio::spawn(
            node.clone()
                .run_accept_loop_without_account_repair(listener),
        );
        let started = tokio::time::timeout(Duration::from_secs(5), probe.completed.recv())
            .await
            .is_ok_and(|v| v.is_some());
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        while probe.completed.try_recv().is_ok() {}
        let before = probe.scans.load(Ordering::SeqCst);
        node.roster
            .lock()
            .unwrap()
            .update(&proof, ip, &node.user_id());
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        let ordinary = probe.scans.load(Ordering::SeqCst);
        // These authoritative leases use std::Instant. Real time deliberately
        // crosses15s public expiry while staying below30s route pruning.
        tokio::time::sleep(crate::discovery::roster::PUBLIC_PRESENCE_TTL + Duration::from_secs(1))
            .await;
        let before_refresh = probe.scans.load(Ordering::SeqCst);
        let expired = node
            .roster
            .lock()
            .unwrap()
            .get(&peer.public().user_id())
            .is_some_and(|record| record.presence_deadline(Instant::now()).is_none());
        let fresh_at = tokio::time::Instant::now();
        node.roster
            .lock()
            .unwrap()
            .update(&proof, ip, &node.user_id());
        let renewed = tokio::time::timeout(Duration::from_secs(1), probe.completed.recv())
            .await
            .is_ok_and(|v| v.is_some());
        let after = probe.scans.load(Ordering::SeqCst);
        stop_control_retry_owner(&node, worker).await;
        assert!(started && before > 0);
        assert_eq!(
            ordinary, before,
            "an unexpired identical heartbeat is quiet"
        );
        assert!(
            expired && before_refresh == ordinary,
            "the owner did not observe/replay an intermediate offline state"
        );
        assert!(
            renewed
                && after > before_refresh
                && fresh_at.elapsed() < CHANNEL_RECOVERY_QUIET_INTERVAL,
            "identical endpoint/proof with newly renewed availability promptly replays retained controls"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn accept_owner_policy_only_change_replays_retained_control_without_public_roster_wake() {
        use std::sync::atomic::Ordering;
        let dir = tempfile::tempdir().unwrap();
        let node = control_retry_node(dir.path());
        node.configure_privacy(
            dir.path(),
            "pw",
            &node.signed_announce("owner", 9),
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
        let channel = node
            .create_account_channel_invited_if("policy replay", Vec::new(), |commit| commit())
            .await
            .unwrap();
        node.set_channel_replica_enabled(channel, false)
            .await
            .unwrap();
        let mut probe = control_retry_probe(&node);
        let mut maintenance = node.storage.database().subscribe_maintenance_changes();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let worker = tokio::spawn(
            node.clone()
                .run_accept_loop_without_account_repair(listener),
        );
        let started = tokio::time::timeout(Duration::from_secs(5), probe.completed.recv())
            .await
            .is_ok_and(|v| v.is_some());
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        while probe.completed.try_recv().is_ok() {}
        maintenance.borrow_and_update();
        let before = probe.scans.load(Ordering::SeqCst);
        let generation = node.privacy.generation.load(Ordering::Acquire);
        let changed_at = tokio::time::Instant::now();
        node.set_invisible(true).await.unwrap();
        let committed = node.privacy.generation.load(Ordering::Acquire) > generation;
        let retained = maintenance.has_changed().unwrap();
        let empty_roster = node.roster.lock().unwrap().peers().is_empty();
        let replayed = tokio::time::timeout(Duration::from_secs(1), probe.completed.recv())
            .await
            .is_ok_and(|v| v.is_some());
        let after = probe.scans.load(Ordering::SeqCst);
        stop_control_retry_owner(&node, worker).await;
        assert!(
            started && before > 0 && committed && retained && empty_roster,
            "actual privacy commit publishes retained dependency wake without public presence"
        );
        assert!(
            replayed && after > before && changed_at.elapsed() < CHANNEL_RECOVERY_QUIET_INTERVAL,
            "policy-only changes promptly revalidate retained control replay before sparse repair"
        );
    }

    #[tokio::test]
    async fn channel_custody_noop_sync_confirmation_does_not_notify_delivery_status() {
        custody_notification_probe_fixture(true).await;
    }

    #[tokio::test]
    async fn channel_custody_noop_recheck_does_not_notify_delivery_status() {
        custody_notification_probe_fixture(false).await;
    }

    #[tokio::test(start_paused = true)]
    async fn local_channel_owner_notify_advances_local_work_without_restarting_held_remote() {
        use crate::{identity::account::Account, node::test_support::node_with_files};
        use futures_util::FutureExt;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let proof = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "fixture",
            9,
        );
        let (node, _, _) = node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let entered = Arc::new(AtomicUsize::new(0));
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let entered = entered.clone();
            move |_, phase| {
                if phase == "channel_recovery_plaintext_done" {
                    entered.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
        let (release, held) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn({
            let node = node.clone();
            async move {
                let remote = async {
                    let _ = held.await;
                    LocalRecoveryWork::default()
                };
                tokio::pin!(remote);
                node.complete_channel_recovery_turn(remote.as_mut()).await
            }
        });
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        node.channel_recovery_notify.notify_one();
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
        let local_progress = entered.load(Ordering::SeqCst);
        let held_alive = !worker.is_finished();
        release.send(()).unwrap();
        let _ = worker.await.unwrap();
        assert!(
            local_progress > 0,
            "local SQL work progresses before remote release"
        );
        assert!(
            held_alive,
            "the same remote remains owned until explicitly released"
        );
        assert!(
            node.channel_recovery_notify
                .notified()
                .now_or_never()
                .is_some(),
            "held-turn notification retains an outer turn"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn local_channel_owner_drains_history_tail_and_metadata_cache_capacity_then_settles() {
        use crate::{identity::account::Account, node::test_support::node_with_files};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let proof = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "fixture",
            9,
        );
        let (node, _, _) = node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        for _ in 0..65 {
            let channel = node
                .create_channel("bounded maintenance", Vec::new())
                .await
                .unwrap();
            node.set_channel_announcement_if(
                channel,
                Some("tiny current publication"),
                None,
                |commit| commit(),
            )
            .await
            .unwrap();
        }
        let entered = Arc::new(AtomicUsize::new(0));
        let history = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let lanes = Arc::new(std::sync::Mutex::new(HashMap::<String, usize>::new()));
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let entered = entered.clone();
            let history = history.clone();
            let lanes = lanes.clone();
            move |_, phase| {
                if (phase.ends_with("_entered") && phase.starts_with("channel_recovery_"))
                    || phase == "channel_recovery_fallback_begin"
                {
                    *lanes.lock().unwrap().entry(phase.to_owned()).or_default() += 1;
                }
                if phase == "channel_recovery_plaintext_done" {
                    entered.fetch_add(1, Ordering::SeqCst);
                }
                if let Some(channel) = phase.strip_prefix("channel_recovery_history_selected ") {
                    history.lock().unwrap().insert(channel.to_owned());
                }
            }
        }));
        let worker = tokio::spawn(
            node.clone()
                .run_local_channel_recovery_loop(Arc::new(Notify::new()), true),
        );
        for _ in 0..4096 {
            tokio::task::yield_now().await;
        }
        let before = entered.load(Ordering::SeqCst);
        let visited = history.lock().unwrap().len();
        let before_lanes = lanes.lock().unwrap().clone();
        let indexed: i64 = node
            .channel_checkpoints
            .profile()
            .database()
            .read(|c| {
                Ok(
                    c.query_row("SELECT COUNT(*) FROM group_metadata_index", [], |row| {
                        row.get(0)
                    })?,
                )
            })
            .unwrap();
        tokio::time::advance(Duration::from_secs(9)).await;
        for _ in 0..512 {
            tokio::task::yield_now().await;
        }
        let after = entered.load(Ordering::SeqCst);
        let after_lanes = lanes.lock().unwrap().clone();
        worker.abort();
        let _ = worker.await;
        eprintln!(
            "valid65 owner turns {before}->{after}; entered lanes before={before_lanes:?} after={after_lanes:?}"
        );
        assert_eq!(
            indexed, 65,
            "the actual metadata index exceeds its 64-entry reconciliation cache; fixture cannot skip the v5 owner"
        );
        assert_eq!(
            visited, 65,
            "all history rows drain before sparse repair, not only the first channel"
        );
        assert_eq!(
            before, after,
            "65 reconciled metadata channels must reach a fixed point despite the 64-entry cache"
        );
        assert_eq!(
            before_lanes, after_lanes,
            "every local lane retains its completed fixed point"
        );
        let history_turns = before_lanes
            .get("channel_recovery_history_entered")
            .copied()
            .unwrap_or(0);
        let membership_turns = before_lanes
            .get("channel_recovery_membership_entered")
            .copied()
            .unwrap_or(0);
        assert!(history_turns >= 65);
        assert!(
            membership_turns > 0 && membership_turns < history_turns,
            "settled membership is not re-enumerated for every history continuation"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn local_channel_owner_retries_real_discovery_failure_then_recovers_on_notify() {
        use crate::{identity::account::Account, node::test_support::node_with_files};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let proof = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "fixture",
            9,
        );
        let (node, _, _) = node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        node.create_channel("discovery failure", Vec::new())
            .await
            .unwrap();
        node.channel_checkpoints.profile().database().transaction(|c| {
            c.execute_batch("CREATE TEMP TRIGGER fail_channel_discovery BEFORE UPDATE ON channel_source_scan BEGIN SELECT RAISE(ABORT,'injected discovery failure'); END;")?;
            Ok(())
        }).unwrap();
        let failures = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(AtomicUsize::new(0));
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let failures = failures.clone();
            let entered = entered.clone();
            move |_, phase| {
                if phase == "channel_recovery_local_failure" {
                    failures.fetch_add(1, Ordering::SeqCst);
                }
                if phase.starts_with("channel_recovery_runtime_queue ") {
                    entered.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
        let worker = tokio::spawn(
            node.clone()
                .run_local_channel_recovery_loop(Arc::new(Notify::new()), false),
        );
        for _ in 0..256 {
            tokio::task::yield_now().await;
        }
        let first = failures.load(Ordering::SeqCst);
        tokio::time::advance(Duration::from_secs(2)).await;
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        let early = failures.load(Ordering::SeqCst);
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        let retried = failures.load(Ordering::SeqCst);
        node.channel_checkpoints
            .profile()
            .database()
            .transaction(|c| {
                c.execute_batch("DROP TRIGGER fail_channel_discovery")?;
                Ok(())
            })
            .unwrap();
        node.channel_recovery_notify.notify_one();
        for _ in 0..512 {
            tokio::task::yield_now().await;
        }
        let recovered = entered.load(Ordering::SeqCst);
        let settled = failures.load(Ordering::SeqCst);
        tokio::time::advance(Duration::from_secs(9)).await;
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        let final_failures = failures.load(Ordering::SeqCst);
        worker.abort();
        let _ = worker.await;
        assert!(first > 0, "a real SQL error was injected");
        assert_eq!(early, first, "failure backoff does not admit an early turn");
        assert!(
            retried > early,
            "failure retries at its owned three-second deadline"
        );
        assert!(
            recovered > 0,
            "Notify resumes successful discovery without waiting for quiet repair"
        );
        assert_eq!(final_failures, settled);
    }

    #[tokio::test(start_paused = true)]
    async fn local_channel_owner_revisits_existing_lower_row_and_postcut_arrival_without_spinning()
    {
        use crate::{identity::account::Account, node::test_support::node_with_files};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let proof = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "fixture",
            9,
        );
        let (node, _, _) = node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let mut channels = Vec::new();
        for _ in 0..3 {
            channels.push(
                node.create_channel("blocked current channel", Vec::new())
                    .await
                    .unwrap(),
            );
        }
        channels.sort_by_key(|id| *id.as_bytes());
        for channel in &channels {
            node.channel_checkpoints.queue_runtime(*channel).unwrap();
            node.defer_channel_plaintext_for_test(
                *channel,
                std::time::Instant::now() + Duration::from_secs(60),
            );
        }
        let after_cut = ConversationId::new([255; 32]);
        assert!(after_cut.as_bytes() > channels.last().unwrap().as_bytes());
        let visits = Arc::new(std::sync::Mutex::new(HashMap::<String, usize>::new()));
        let injected = Arc::new(AtomicBool::new(false));
        let turns = Arc::new(AtomicUsize::new(0));
        let weak = Arc::downgrade(&node);
        let second = format!("{:?}", channels[1]);
        let first_channel = channels[0];
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let visits = visits.clone();
            let injected = injected.clone();
            let turns = turns.clone();
            move |_, phase| {
                if phase == "channel_recovery_plaintext_done" {
                    turns.fetch_add(1, Ordering::SeqCst);
                }
                if let Some(channel) = phase.strip_prefix("channel_recovery_plaintext_selected ") {
                    *visits
                        .lock()
                        .unwrap()
                        .entry(channel.to_owned())
                        .or_default() += 1;
                    if channel == second && !injected.swap(true, Ordering::SeqCst) {
                        let node = weak.upgrade().unwrap();
                        // Existing blocked identity: enqueue is deliberately a durable no-op.
                        node.channel_checkpoints
                            .queue_runtime(first_channel)
                            .unwrap();
                        node.channel_checkpoints.queue_runtime(after_cut).unwrap();
                        node.channel_recovery_notify.notify_one();
                    }
                }
            }
        }));
        let worker = tokio::spawn(
            node.clone()
                .run_local_channel_recovery_loop(Arc::new(Notify::new()), true),
        );
        for _ in 0..2048 {
            tokio::task::yield_now().await;
        }
        let before = turns.load(Ordering::SeqCst);
        let observed = visits.lock().unwrap().clone();
        tokio::time::advance(Duration::from_secs(9)).await;
        for _ in 0..256 {
            tokio::task::yield_now().await;
        }
        let after = turns.load(Ordering::SeqCst);
        worker.abort();
        let _ = worker.await;
        assert!(injected.load(Ordering::SeqCst));
        assert!(
            observed
                .get(&format!("{first_channel:?}"))
                .copied()
                .unwrap_or(0)
                >= 2,
            "already visited existing work is revisited after a held cut"
        );
        assert!(
            observed.contains_key(&format!("{after_cut:?}")),
            "post-cut demand owns a later finite sweep"
        );
        assert_eq!(
            before, after,
            "blocked rows do not turn pending into immediate progress forever"
        );
    }
    #[tokio::test]
    async fn local_channel_owner_retains_file_discovery_failure_and_recovers_without_new_source() {
        use crate::{identity::account::Account, node::test_support::node_with_files};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let proof = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "fixture",
            9,
        );
        let (node, _, _) = node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let channel = node
            .create_channel("file failure demand", Vec::new())
            .await
            .unwrap();
        node.channel_checkpoints.profile().database().transaction(|c| {
            c.execute_batch("UPDATE file_receive_scan SET after_commit=NULL; CREATE TEMP TRIGGER fail_channel_file_scan BEFORE UPDATE ON file_receive_scan BEGIN SELECT RAISE(ABORT,'injected file discovery failure'); END;")?;
            Ok(())
        }).unwrap();
        let failures = Arc::new(AtomicUsize::new(0));
        let turns = Arc::new(AtomicUsize::new(0));
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let failures = failures.clone();
            let turns = turns.clone();
            move |_, phase| {
                if phase == "channel_recovery_file_failure" {
                    failures.fetch_add(1, Ordering::SeqCst);
                }
                if phase == "channel_recovery_plaintext_done" {
                    turns.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
        let worker = tokio::spawn(
            node.clone()
                .run_local_channel_recovery_loop(Arc::new(Notify::new()), false),
        );
        for _ in 0..256 {
            tokio::task::yield_now().await;
        }
        let first = failures.load(Ordering::SeqCst);
        let retained = node
            .channel_checkpoints
            .next_runtime(None)
            .unwrap()
            .is_some_and(|(queued, _)| queued == channel);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let early = failures.load(Ordering::SeqCst);
        let retried = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let count = failures.load(Ordering::SeqCst);
                if count > first {
                    break count;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        let retried_count = retried.as_ref().copied().unwrap_or(first);
        node.channel_checkpoints
            .profile()
            .database()
            .transaction(|c| {
                c.execute_batch("DROP TRIGGER fail_channel_file_scan")?;
                Ok(())
            })
            .unwrap();
        node.channel_recovery_notify.notify_one();
        let completed = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if node
                    .channel_checkpoints
                    .next_runtime(None)
                    .unwrap()
                    .is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok();
        let before = turns.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(200)).await;
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        let after = turns.load(Ordering::SeqCst);
        worker.abort();
        let _ = worker.await;
        assert!(
            first > 0,
            "a real file-discovery SQL error reached the owner"
        );
        assert!(
            retained,
            "file failure restores durable demand even after plaintext completes"
        );
        assert_eq!(
            early, first,
            "a real per-channel deadline prevents early I/O retry"
        );
        assert!(
            retried_count > first,
            "file I/O failure retains its own retry without periodic repair"
        );
        assert!(
            completed,
            "successful retry retires the exact queued demand"
        );
        assert_eq!(
            before, after,
            "successful repair reaches a quiet fixed point"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn local_channel_owner_field_read_failure_recovery_publishes_one_ui_refresh() {
        use crate::{identity::account::Account, node::test_support::node_with_files};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let proof = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "fixture",
            9,
        );
        let (node, _, _) = node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let channel = node
            .create_channel("field recovery", Vec::new())
            .await
            .unwrap();
        node.set_channel_announcement_if(channel, Some("visible after recovery"), None, |commit| {
            commit()
        })
        .await
        .unwrap();
        for _ in 0..128 {
            node.retry_channel_plaintext();
        }
        let mut ready = false;
        for _ in 0..128 {
            if !node.channel_metadata(channel).unwrap().query_pending {
                ready = true;
                break;
            }
        }
        assert!(
            ready,
            "the durable index has already published its ready transition"
        );
        let source = node.channel_checkpoints.source_binding(channel).unwrap();
        node.channel_checkpoints
            .profile()
            .database()
            .transaction(|c| {
                c.execute_batch(
                    "ALTER TABLE group_metadata_publications RENAME TO held_metadata_publications;",
                )?;
                Ok(())
            })
            .unwrap();
        node.clear_metadata_display_values_for_test();
        let consumer_failed = node.channel_metadata(channel).is_err();
        let failures = Arc::new(AtomicUsize::new(0));
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let failures = failures.clone();
            move |_, phase| {
                if phase == "channel_recovery_metadata_failed" {
                    failures.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
        let mut ui = node.subscribe_ui_changes();
        let worker = tokio::spawn(
            node.clone()
                .run_local_channel_recovery_loop(Arc::new(Notify::new()), true),
        );
        for _ in 0..512 {
            tokio::task::yield_now().await;
        }
        let failed = failures.load(Ordering::SeqCst);
        ui.borrow_and_update();
        node.channel_checkpoints
            .profile()
            .database()
            .transaction(|c| {
                c.execute_batch(
                    "ALTER TABLE held_metadata_publications RENAME TO group_metadata_publications;",
                )?;
                Ok(())
            })
            .unwrap();
        tokio::time::advance(Duration::from_secs(3)).await;
        for _ in 0..512 {
            tokio::task::yield_now().await;
        }
        let recovered_ui = ui.has_changed().unwrap();
        let metadata = node.channel_metadata(channel).unwrap();
        ui.borrow_and_update();
        tokio::time::advance(Duration::from_secs(9)).await;
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        let repeated_ui = ui.has_changed().unwrap();
        let after_source = node.channel_checkpoints.source_binding(channel).unwrap();
        worker.abort();
        let _ = worker.await;
        assert!(
            consumer_failed,
            "the actual display query encountered the field I/O failure"
        );
        assert!(
            failed > 0,
            "a real field hydration SELECT failed after index readiness"
        );
        assert!(
            recovered_ui,
            "successful owner retry refreshes consumers that swallowed the first field error"
        );
        assert!(!metadata.query_pending);
        assert_eq!(
            metadata.announcement.unwrap().content.as_deref(),
            Some("visible after recovery")
        );
        assert!(
            source == after_source,
            "recovery is not fabricated source change"
        );
        assert!(
            !repeated_ui,
            "read-only successful followups emit no UI feedback"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn settled_local_channel_owner_does_not_reenter_business_between_repair_deadlines() {
        use crate::{identity::account::Account, node::test_support::node_with_files};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let proof = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "fixture peer",
            9,
        );
        let (node, _, _) = node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        assert_eq!(node.joined_channel_count(), 0);
        let entered = Arc::new(AtomicUsize::new(0));
        let fallbacks = Arc::new(AtomicUsize::new(0));
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let entered = entered.clone();
            let fallbacks = fallbacks.clone();
            move |_, phase| {
                // This existing hook runs only after orphan/discovery/resume and
                // next-runtime SQL completed; it is not a scheduler mirror count.
                if phase.starts_with("channel_recovery_runtime_queue ") {
                    entered.fetch_add(1, Ordering::SeqCst);
                }
                if phase == "channel_recovery_fallback_begin" {
                    fallbacks.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
        let worker = tokio::spawn(
            node.clone()
                .run_local_channel_recovery_loop(Arc::new(Notify::new()), true),
        );
        async fn drain_ready() {
            for _ in 0..64 {
                tokio::task::yield_now().await;
            }
        }
        drain_ready().await;
        // Permit all legitimate cold-start work. The old repair backoff grows
        // through 3/6/12/24 seconds; four 60-second steps put its next repair beyond the
        // following nine-second measurement. A settled 300-second owner has no due repair either.
        for _ in 0..4 {
            tokio::time::advance(Duration::from_secs(60)).await;
            drain_ready().await;
        }
        let baseline = entered.load(Ordering::SeqCst);
        let before_fallbacks = fallbacks.load(Ordering::SeqCst);
        let empty = node
            .channel_checkpoints
            .next_runtime(None)
            .unwrap()
            .is_none()
            && node.channel_recovery_pending.lock().unwrap().is_empty();
        for _ in 0..3 {
            tokio::time::advance(CHANNEL_RECOVERY_INTERVAL).await;
            drain_ready().await;
        }
        let after = entered.load(Ordering::SeqCst);
        let after_fallbacks = fallbacks.load(Ordering::SeqCst);
        worker.abort();
        let _ = worker.await;
        eprintln!(
            "settled local owner measured postSQL turns {baseline}->{after}, fallback turns {before_fallbacks}->{after_fallbacks}, empty={empty}"
        );
        assert!(
            baseline > 0,
            "Actual owner startup must have entered business work"
        );
        assert!(
            empty,
            "Measure only after durable and in-memory queues are empty"
        );
        assert_eq!(
            after_fallbacks, before_fallbacks,
            "No real repair deadline falls within the measured interval"
        );
        assert_eq!(
            after, baseline,
            "An empty settled owner must not reenter post-SQL business work on quiet three-second ticks"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn settled_channel_custody_waits_for_semantic_change_instead_of_three_second_scan() {
        use crate::{identity::account::Account, node::test_support::node_with_files};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "known peer", 12345);
        let (node, _, _) = node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        node.roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.2".parse().unwrap(), &node.user_id());
        let channel = node
            .create_channel("settled local group", Vec::new())
            .await
            .unwrap();
        node.send_channel_message(channel, b"durable startup message")
            .await
            .unwrap();
        let completed = Arc::new(AtomicUsize::new(0));
        let rechecked = Arc::new(AtomicUsize::new(0));
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let completed = completed.clone();
            let rechecked = rechecked.clone();
            move |_, phase| {
                if phase == "channel_recovery_custody_recheck_begin" {
                    rechecked.fetch_add(1, Ordering::SeqCst);
                }
                if phase == "channel_recovery_custody_done" {
                    completed.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
        let worker = tokio::spawn(node.clone().run_channel_custody_recovery_loop(true));
        async fn await_turn(completed: &AtomicUsize, minimum: usize) {
            for _ in 0..128 {
                if completed.load(Ordering::SeqCst) >= minimum {
                    return;
                }
                tokio::task::yield_now().await;
            }
            panic!("custody owner did not complete expected admitted turn");
        }
        let result = async {
            await_turn(&completed, 1).await;
            let initial = completed.load(Ordering::SeqCst);
            tokio::time::advance(Duration::from_secs(15)).await;
            for _ in 0..32 {
                tokio::task::yield_now().await;
            }
            assert_eq!(
                completed.load(Ordering::SeqCst),
                initial,
                "settled unchanged custody must not rescan own-message keys every three seconds"
            );
            for ip in ["127.0.0.1", "127.0.0.2"] {
                node.roster
                    .lock()
                    .unwrap()
                    .update(&proof, ip.parse().unwrap(), &node.user_id());
                for _ in 0..32 {
                    tokio::task::yield_now().await;
                }
                assert_eq!(
                    completed.load(Ordering::SeqCst),
                    initial,
                    "known-route freshness/preference renewal must not admit custody scans"
                );
            }
            tokio::time::advance(Duration::from_secs(45)).await;
            for _ in 0..32 {
                tokio::task::yield_now().await;
            }
            assert!(
                rechecked.load(Ordering::SeqCst) >= 2,
                "old claims retain independent sixty-second repair"
            );
            assert_eq!(
                completed.load(Ordering::SeqCst),
                initial,
                "old-claim recheck must not invoke the 128-row retry scan"
            );
            tokio::time::advance(Duration::from_secs(240)).await;
            await_turn(&completed, initial + 1).await;
            let initial = completed.load(Ordering::SeqCst);
            node.send_channel_message(channel, b"new durable message")
                .await
                .unwrap();
            await_turn(&completed, initial + 1).await;
            let after_message = completed.load(Ordering::SeqCst);
            node.roster.lock().unwrap().update(
                &proof,
                "127.0.0.3".parse().unwrap(),
                &node.user_id(),
            );
            await_turn(&completed, after_message + 1).await;
        };
        use futures_util::FutureExt;
        let outcome = std::panic::AssertUnwindSafe(result).catch_unwind().await;
        worker.abort();
        let _ = worker.await;
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn channel_custody_startup_continues_beyond_one_bounded_page_before_parking() {
        use crate::{identity::account::Account, node::test_support::node_with_files};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let proof = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "peer",
            9,
        );
        let (node, _, _) = node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let channel = node
            .create_channel("startup durable queue", Vec::new())
            .await
            .unwrap();
        for _ in 0..CHANNEL_CUSTODY_SCAN_PER_TICK + 1 {
            node.send_channel_message(channel, b"durable startup message")
                .await
                .unwrap();
        }
        let completed = Arc::new(AtomicUsize::new(0));
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let completed = completed.clone();
            move |_, phase| {
                if phase == "channel_recovery_custody_done" {
                    completed.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
        let worker = tokio::spawn(node.clone().run_channel_custody_recovery_loop(true));
        let result = async {
            for _ in 0..128 {
                tokio::task::yield_now().await;
            }
            assert_eq!(completed.load(Ordering::SeqCst), 1);
            tokio::time::advance(CHANNEL_RECOVERY_INTERVAL).await;
            for _ in 0..128 {
                tokio::task::yield_now().await;
            }
            assert_eq!(
                completed.load(Ordering::SeqCst),
                2,
                "a full first page cannot acknowledge the unvisited startup tail"
            );
            tokio::time::advance(Duration::from_secs(15)).await;
            for _ in 0..128 {
                tokio::task::yield_now().await;
            }
            assert_eq!(
                completed.load(Ordering::SeqCst),
                2,
                "the completed unchanged sweep must then become quiet"
            );
        };
        use futures_util::FutureExt;
        let outcome = std::panic::AssertUnwindSafe(result).catch_unwind().await;
        worker.abort();
        let _ = worker.await;
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn channel_custody_input_change_requires_complete_sweep_from_retained_cursor() {
        use crate::{identity::account::Account, node::test_support::node_with_files};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let proof = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "peer",
            9,
        );
        let (node, _, _) = node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let channel = node
            .create_channel("generation-qualified startup sweep", Vec::new())
            .await
            .unwrap();
        for _ in 0..CHANNEL_CUSTODY_SCAN_PER_TICK * 2 + 1 {
            node.send_channel_message(channel, b"durable startup message")
                .await
                .unwrap();
        }
        let completed = Arc::new(AtomicUsize::new(0));
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let completed = completed.clone();
            move |_, phase| {
                if phase == "channel_recovery_custody_done" {
                    completed.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
        let worker = tokio::spawn(node.clone().run_channel_custody_recovery_loop(true));
        async fn drain_ready() {
            for _ in 0..128 {
                tokio::task::yield_now().await;
            }
        }
        let result = async {
            drain_ready().await;
            assert_eq!(completed.load(Ordering::SeqCst), 1);
            // The retained fair cursor is now #128. A new source generation
            // must cover that boundary too, not settle on the earlier key wrap.
            node.send_channel_message(channel, b"new source generation")
                .await
                .unwrap();
            drain_ready().await;
            assert_eq!(completed.load(Ordering::SeqCst), 2);
            tokio::time::advance(CHANNEL_RECOVERY_INTERVAL).await;
            drain_ready().await;
            assert_eq!(completed.load(Ordering::SeqCst), 3);
            tokio::time::advance(CHANNEL_RECOVERY_INTERVAL).await;
            drain_ready().await;
            assert_eq!(
                completed.load(Ordering::SeqCst),
                4,
                "wrapping to #1 cannot settle a generation whose scan began after #128; the boundary must be reconsidered"
            );
            tokio::time::advance(Duration::from_secs(15)).await;
            drain_ready().await;
            assert_eq!(
                completed.load(Ordering::SeqCst),
                4,
                "a completed current-generation sweep must then park"
            );
        };
        use futures_util::FutureExt;
        let outcome = std::panic::AssertUnwindSafe(result).catch_unwind().await;
        worker.abort();
        let _ = worker.await;
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    #[tokio::test]
    async fn channel_custody_sweeps_accumulate_across_offset_channel_cursors() {
        use crate::{identity::account::Account, node::test_support::node_with_files};
        let dir = tempfile::tempdir().unwrap();
        let proof = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "peer",
            9,
        );
        let (node, _, _) = node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let mut channels = Vec::new();
        for name in ["first quiet sweep", "offset quiet sweep"] {
            let channel = node.create_channel(name, Vec::new()).await.unwrap();
            for _ in 0..CHANNEL_CUSTODY_SCAN_PER_TICK * 3 {
                node.send_channel_message(channel, b"quiet durable startup message")
                    .await
                    .unwrap();
            }
            channels.push(channel);
        }
        channels.sort_unstable_by_key(|id| *id.as_bytes());
        let keys = node
            .log
            .lock()
            .unwrap()
            .message_keys_after(
                channels[1],
                crate::eventlog::event::Author::from_ed25519(node.identity.public().ed25519_pub),
                None,
                CHANNEL_CUSTODY_SCAN_PER_TICK * 3,
            )
            .unwrap();
        let mut cursors = HashMap::from([(
            channels[1],
            CustodyRetryCursor {
                after: Some(keys[CHANNEL_CUSTODY_SCAN_PER_TICK * 2 - 1]),
                ..Default::default()
            },
        )]);
        let mut progress = CustodyRetryProgress::default();
        for turn in 0..8 {
            progress = CustodyRetryProgress::default();
            assert!(
                node.retry_channel_custody_turn(turn, &mut cursors, 1, &mut progress)
                    .await
                    .is_none()
            );
            if turn == 5 {
                assert!(cursors[&channels[1]].complete);
                assert!(!cursors[&channels[0]].complete);
            }
            if turn > 5 {
                assert!(
                    cursors[&channels[1]].complete,
                    "another channel's unfinished page cannot erase completed sweep qualification"
                );
            }
        }
        assert!(
            progress.quiet_complete,
            "independently completed current-generation sweeps must park"
        );
        assert_eq!(cursors.len(), 2);
    }

    #[test]
    fn channel_custody_cursor_qualifies_generation_boundary_without_resetting_fair_position() {
        let key = |lamport| (lamport, EventId::new([lamport as u8; 32]));
        let mut state = CustodyRetryCursor {
            after: Some(key(128)),
            ..Default::default()
        };
        state.restart_sweep();
        assert!(!state.advance(key(256)).1);
        assert!(
            !state.advance(key(1)).1,
            "first wrap does not revisit the captured boundary"
        );
        assert!(!state.advance(key(127)).1);
        assert!(state.advance(key(128)).1);
        assert_eq!(state.after, Some(key(128)));
        state.restart_sweep();
        assert_eq!(
            state.after,
            Some(key(128)),
            "invalidation preserves fair position"
        );
        assert!(!state.advance(key(1)).1);
        assert!(!state.advance(key(127)).1);
        assert!(
            state.advance(key(1)).1,
            "a removed boundary completes after the next full traversal"
        );
    }

    #[test]
    fn channel_custody_batch_verifier_keys_full_certificate_and_bounds_retention() {
        use crate::{
            discovery::Announce, identity::account::Account, node::test_support::node_with_files,
        };
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let identity = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&identity, &account, "verifier", 9);
        let (node, _, _) = node_with_files(dir.path(), identity, account, &proof);
        let holder = DeviceIdentity::generate();
        let certificate = node.account.certify(&holder.public().ed25519_pub);
        let mut verifier = CustodyCertificateVerifier::default();
        assert!(verifier.verify(&node, &holder.public(), &certificate));
        let mut changed = certificate.clone();
        changed.signature[0] ^= 1;
        assert!(!verifier.verify(&node, &holder.public(), &changed));
        changed = certificate.clone();
        changed.account_ed25519_pub[0] ^= 1;
        assert!(!verifier.verify(&node, &holder.public(), &changed));
        changed = certificate.clone();
        changed.device_ed25519_pub[0] ^= 1;
        assert!(!verifier.verify(&node, &holder.public(), &changed));
        for value in 0u16..300 {
            let mut changed = certificate.clone();
            changed.signature[..2].copy_from_slice(&value.to_le_bytes());
            verifier.verify(&node, &holder.public(), &changed);
        }
        assert_eq!(verifier.verified.len(), 256);
        // Cache authority covers signatures only, never the asserted holder.
        assert!(verifier.verify(&node, &node.identity.public(), &certificate));
        assert_ne!(
            certificate.device_ed25519_pub,
            node.identity.public().ed25519_pub
        );
    }

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

    #[test]
    fn applied_group_continuations_rotate_past_requeued_low_ids_and_deleted_cursor() {
        let groups: Vec<_> = (0..17).map(|id| ConversationId::new([id; 32])).collect();
        let mut pending: HashSet<_> = groups.iter().copied().collect();
        let mut last = None;
        let first = take_channel_recovery_targets(&mut pending, &mut last);
        assert_eq!(
            first,
            groups[..8].iter().map(|id| (*id, 0)).collect::<Vec<_>>()
        );
        pending.extend(first.into_iter().map(|(id, _)| id));
        assert_eq!(
            take_channel_recovery_targets(&mut pending, &mut last),
            groups[8..16].iter().map(|id| (*id, 0)).collect::<Vec<_>>()
        );
        let third = take_channel_recovery_targets(&mut pending, &mut last);
        let expected: Vec<_> = std::iter::once((groups[16], 0))
            .chain(groups[..7].iter().map(|id| (*id, 1)))
            .collect();
        assert_eq!(
            third, expected,
            "the seventeenth group precedes wrap, and the cycle changes inside the batch"
        );
        assert_eq!(pending, HashSet::from([groups[7]]));
        pending = HashSet::from([groups[2], groups[9]]);
        assert_eq!(last, Some((groups[6], 1)));
        assert_eq!(
            take_channel_recovery_targets(&mut pending, &mut last),
            vec![(groups[9], 1), (groups[2], 2)]
        );
        assert!(pending.is_empty());
        assert!(take_channel_recovery_targets(&mut pending, &mut last).is_empty());
        assert_eq!(
            last,
            Some((groups[2], 2)),
            "an empty poll retains identity and cycle"
        );
    }

    #[test]
    fn hot_group_sweeps_do_not_alias_four_peer_rotation_across_four_batches() {
        let groups: Vec<_> = (0..32).map(|id| ConversationId::new([id; 32])).collect();
        let mut pending: HashSet<_> = groups.iter().copied().collect();
        let mut last = None;
        let mut cycles: HashMap<_, Vec<usize>> = HashMap::new();
        let mut peers: HashMap<_, HashSet<usize>> = HashMap::new();
        for _ in 0..16 {
            let batch = take_channel_recovery_targets(&mut pending, &mut last);
            assert_eq!(batch.len(), CHANNEL_RECOVERY_WAKE_BATCH);
            for &(group, cycle) in &batch {
                cycles.entry(group).or_default().push(cycle);
                let rotation = usize::from(group.as_bytes()[0]);
                let start = rotation.wrapping_add(cycle) % 4;
                for offset in 0..CHANNEL_RECOVERY_PEERS_PER_TICK {
                    peers.entry(group).or_default().insert((start + offset) % 4);
                }
            }
            // Every group stays hot: the four-batch schedule must not cause
            // a group's peer start to advance by four modulo four forever.
            pending.extend(batch.into_iter().map(|(group, _)| group));
        }
        for group in groups {
            assert_eq!(cycles[&group], vec![0, 1, 2, 3]);
            assert_eq!(
                peers[&group],
                HashSet::from([0, 1, 2, 3]),
                "all four peers must receive a turn despite three-peer rounds"
            );
        }
    }

    #[tokio::test]
    async fn applied_metadata_only_recovery_retains_one_fresh_group_turn_without_quiet_spin() {
        use crate::{
            identity::account::Account,
            node::test_support::{node_with_files, settle_group_metadata},
        };
        use futures_util::FutureExt;
        let source_dir = tempfile::tempdir().unwrap();
        let recipient_dir = tempfile::tempdir().unwrap();
        let source_identity = DeviceIdentity::generate();
        let recipient_identity = DeviceIdentity::generate();
        let source_account = Account::generate();
        let recipient_account = Account::generate();
        let source_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let recipient_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_proof = Announce::new_with_account(
            &source_identity,
            &source_account,
            "source",
            source_listener.local_addr().unwrap().port(),
        );
        let recipient_proof = Announce::new_with_account(
            &recipient_identity,
            &recipient_account,
            "recipient",
            recipient_listener.local_addr().unwrap().port(),
        );
        let (source, _, _) = node_with_files(
            source_dir.path(),
            source_identity,
            source_account,
            &recipient_proof,
        );
        let (recipient, _, _) = node_with_files(
            recipient_dir.path(),
            recipient_identity,
            recipient_account,
            &source_proof,
        );
        // Serve real Noise sessions without starting an independent recovery
        // consumer that could consume the pending work being asserted here.
        let mut servers = tokio::task::JoinSet::new();
        for (node, listener) in [
            (source.clone(), source_listener),
            (recipient.clone(), recipient_listener),
        ] {
            servers.spawn(async move {
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    let channel = node.privacy_accept(stream).await.unwrap();
                    node.serve_connection(channel).await;
                }
            });
        }
        let observed = std::panic::AssertUnwindSafe(async {
            let group = source.create_channel("original", vec![recipient.identity.public()]).await.unwrap();
            settle_group_metadata(&source, group);
            settle_group_metadata(&recipient, group);
            assert_eq!(recipient.channels.lock().unwrap().state(&group).unwrap().name(), "original");
            // Produce a real owner-signed rename through its public owner,
            // withholding only the push route so recovery must fetch it.
            source.roster.lock().unwrap().forget(&recipient.identity.public());
            source.rename_channel(group, "metadata-only progress").await.unwrap();
            settle_group_metadata(&source, group);
            recipient.channel_recovery_pending.lock().unwrap().clear();
            assert!(recipient.channel_history(group, 10).unwrap().is_empty());
            recipient.pull_channel_from_members(group, 0, false).await;
            settle_group_metadata(&recipient, group);
            assert_eq!(recipient.channels.lock().unwrap().state(&group).unwrap().name(), "metadata-only progress", "the real signed metadata must have been applied through Noise");
            assert!(recipient.channel_history(group, 10).unwrap().is_empty(), "metadata-only progress must not manufacture a NewChannelMessage wake");
            assert!(recipient.channel_recovery_pending.lock().unwrap().remove(&group), "applied metadata-only progress must retain this group's fresh recovery turn after aborting stale peer children");
            recipient.pull_channel_from_members(group, 1, false).await;
            settle_group_metadata(&recipient, group);
            assert!(!recipient.channel_recovery_pending.lock().unwrap().contains(&group), "an identical quiet sync must not requeue the group and spin");
        }).catch_unwind().await;
        for node in [&source, &recipient] {
            node.runtime_work.close();
            node.sessions.clear();
        }
        servers.abort_all();
        while servers.join_next().await.is_some() {}
        for node in [&source, &recipient] {
            node.runtime_work.drain().await;
        }
        if let Err(panic) = observed {
            std::panic::resume_unwind(panic);
        }
    }

    #[tokio::test(start_paused = true)]
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
        // Establish the real owner's settled startup before observing its
        // sparse quiet-repair deadline. No peer/transport failure owns a retry.
        for _ in 0..256 {
            tokio::task::yield_now().await;
        }
        let startup = node.channel_recovery.lock().unwrap().contains_key(&channel);
        node.channel_recovery.lock().unwrap().clear();
        tokio::time::advance(CHANNEL_RECOVERY_QUIET_INTERVAL - Duration::from_secs(1)).await;
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        let early = node.channel_recovery.lock().unwrap().contains_key(&channel);
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..256 {
            tokio::task::yield_now().await;
        }
        let repaired = node.channel_recovery.lock().unwrap().contains_key(&channel);
        node.channel_recovery.lock().unwrap().clear();
        tokio::time::advance(CHANNEL_RECOVERY_QUIET_INTERVAL - Duration::from_secs(1)).await;
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        let before_network = node.channel_recovery.lock().unwrap().contains_key(&channel);
        let notified_at = tokio::time::Instant::now();
        node.network_changes
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        for _ in 0..256 {
            tokio::task::yield_now().await;
        }
        let restored = node.channel_recovery.lock().unwrap().contains_key(&channel);
        let completed_at = tokio::time::Instant::now();
        worker.abort();
        let _ = worker.await;
        assert!(startup, "startup repair visits the joined channel");
        assert!(
            !early,
            "settled owner does not repair before its quiet deadline"
        );
        assert!(
            repaired,
            "the fallback at 300 seconds still repairs without notifications"
        );
        assert!(
            !before_network,
            "the next quiet deadline has not yet arrived"
        );
        assert!(
            restored,
            "network restoration repairs immediately despite the quiet deadline"
        );
        assert_eq!(
            notified_at, completed_at,
            "network wake requires no timer advancement"
        );
    }

    #[tokio::test(start_paused = true)]
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
        // Creation notifications precede subscription. Startup must drain the
        // captured sweep of 12 groups in bounded turns of 8 and 4 groups before it can settle.
        let worker = tokio::spawn(
            node.clone()
                .run_channel_recovery_loop(Arc::new(Notify::new())),
        );
        for _ in 0..512 {
            tokio::task::yield_now().await;
        }
        let startup = node.channel_recovery.lock().unwrap().len();
        node.channel_recovery.lock().unwrap().clear();
        let target = channels[11];
        for _ in 0..1000 {
            node.schedule_channel_recovery(target);
        }
        let coalesced = node.channel_recovery_pending.lock().unwrap().len();
        for _ in 0..256 {
            tokio::task::yield_now().await;
        }
        let targeted = node.channel_recovery.lock().unwrap().contains_key(&target);
        let unrelated = node
            .channel_recovery
            .lock()
            .unwrap()
            .contains_key(&channels[10]);
        tokio::time::advance(CHANNEL_RECOVERY_QUIET_INTERVAL - Duration::from_secs(1)).await;
        for _ in 0..128 {
            tokio::task::yield_now().await;
        }
        let early_unrelated = node
            .channel_recovery
            .lock()
            .unwrap()
            .contains_key(&channels[10]);
        // No further notification is sent. At the owned repair deadline the
        // rotating fallback must still visit every group, including the tail.
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..512 {
            tokio::task::yield_now().await;
        }
        let repaired = node.channel_recovery.lock().unwrap().len();
        worker.abort();
        let _ = worker.await;
        assert_eq!(
            startup,
            channels.len(),
            "all lost startup notifications are covered by the finite sweep"
        );
        assert_eq!(
            coalesced, 1,
            "1,000 wakes for one group retain one pending target"
        );
        assert!(
            targeted,
            "targeted wake visits its group before rotating repair"
        );
        assert!(
            !unrelated && !early_unrelated,
            "target priority does not create early unrelated repair"
        );
        assert_eq!(
            repaired,
            channels.len(),
            "deadline repair reconciles all groups without new notifications"
        );
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
        crate::node::test_support::settle_group_metadata(&node, channel);
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
    fn custody_retry_classes_each_rotate_through_all_competing_peers() {
        for peer_count in 2..=4 {
            let peers: Vec<_> = (0..peer_count)
                .map(|port| PeerRecord {
                    public: DeviceIdentity::generate().public(),
                    addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 4300 + port)),
                    routes: Vec::new(),
                    name: String::new(),
                    post_office: false,
                    account_id: None,
                    last_seen: Instant::now(),
                })
                .collect();
            let mut replication = HashSet::new();
            let mut reports = HashSet::new();
            for cycle in 0..2 * peers.len() {
                let (report_first, peer_cycle) = custody_retry_order(cycle);
                let peer = select_custody_peer(peers.clone(), peer_cycle).unwrap();
                if report_first {
                    reports.insert(peer.public.user_id());
                } else {
                    replication.insert(peer.public.user_id());
                }
            }
            assert_eq!(replication.len(), peers.len());
            assert_eq!(reports.len(), peers.len());
            assert_eq!(replication, reports);
        }
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
