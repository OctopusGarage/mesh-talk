//! Channel lifecycle: create, membership, sender-key distribution, and sends. Split out of node.rs (one `impl Node` block per domain).

use super::node::now_millis;
use super::*;
use crate::channel::ChannelMeta;
use crate::discovery::roster::PeerRecord;
use crate::eventlog::event::{ConversationId, EventId, EventKind};
use crate::identity::device::PublicIdentity;
use crate::node::channel::seal_keys_for;
use crate::node::session::{request_durable_have, request_round};
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

impl Node {
    /// Pick one verified account sibling for an owner-signed channel invitation.
    /// Discovery and the durable route cache only expose account ids from verified
    /// device certificates; never infer account ownership from display names.
    pub(in crate::node) fn auto_enrollment_candidate(
        &self,
    ) -> Option<(ConversationId, PublicIdentity)> {
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
            if state.owner() != me || state.is_dissolved() {
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
                    return Some((channel, peer.public.clone()));
                }
            }
        }
        None
    }

    /// Persist an owner-signed invitation for one sibling per pass. The ordinary
    /// channel distribution and recovery paths deliver it to the new device.
    pub(in crate::node) async fn auto_enroll_one_channel_sibling(&self) -> Result<bool, NodeError> {
        let Some((channel, device)) = self.auto_enrollment_candidate() else {
            return Ok(false);
        };
        self.add_channel_member(channel, device).await?;
        Ok(true)
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
        let mut retry_after = HashMap::new();
        let mut retry_cycle = HashMap::new();
        let mut recheck_after = HashMap::new();
        let mut recheck_cycle = HashMap::new();
        let mut startup = true;
        loop {
            let discovered = tokio::select! {
                _ = changes.notified() => true,
                _ = tick.tick() => false,
            };
            if let Err(error) = self.auto_enroll_one_channel_sibling().await {
                log::warn!("linked channel invitation failed; will retry: {error}");
            }
            if let Err(error) = self.auto_accept_one_linked_invite().await {
                log::warn!("linked channel acceptance failed; will retry: {error}");
            }
            let batch = if startup || discovered {
                self.joined_channel_count().min(CHANNEL_RECOVERY_WAKE_BATCH)
            } else {
                1
            };
            startup = false;
            for _ in 0..batch {
                self.pull_one_channel_from_member(cursor).await;
                cursor = cursor.wrapping_add(1);
            }
            self.retry_one_channel_custody(custody_cursor, &mut retry_after, &mut retry_cycle)
                .await;
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
        let members = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
            if !state.is_accepted_member(&self.user_id()) || state.is_dissolved() {
                return Err(NodeError::Channel("not an active channel member".into()));
            }
            state.members().to_vec()
        };
        self.append_event(channel, EventKind::ChannelReplica, vec![u8::from(enabled)])?;
        self.process_channel(channel);
        self.distribute_channel(channel, &members).await;
        Ok(())
    }

    /// Close a channel for every member while retaining each device's local history.
    pub async fn dissolve_channel(&self, channel: ConversationId) -> Result<(), NodeError> {
        let members = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            if self.user_id() != state.owner() {
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
        self.distribute_channel(channel, &members).await;
        Ok(())
    }

    /// Event ids visible to this device after it accepted an invitation.
    /// `None` means this channel has no acceptance cutoff (or is not a channel).
    pub(in crate::node) fn channel_content_gate(
        &self,
        channel: ConversationId,
    ) -> Option<HashSet<EventId>> {
        let status = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            book.state(&channel)
                .map(|state| (state.pending_invite(), state.accepted_event_id()))
        }?;
        if status.0 {
            return Some(HashSet::new());
        }
        let accepted = status.1?;
        let log = self.log.lock().expect("log mutex not poisoned");
        let mut descendants = HashSet::new();
        for event in log.events(&channel) {
            if event.id == accepted
                || event
                    .parents
                    .iter()
                    .any(|parent| descendants.contains(parent))
            {
                descendants.insert(event.id);
            }
        }
        Some(descendants)
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
        self.create_channel_with_consent(name, members, false).await
    }

    /// Desktop-created groups require every invited device to accept explicitly.
    pub async fn create_channel_invited(
        &self,
        name: &str,
        members: Vec<PublicIdentity>,
    ) -> Result<ConversationId, NodeError> {
        self.create_channel_with_consent(name, members, true).await
    }

    async fn create_channel_with_consent(
        &self,
        name: &str,
        mut members: Vec<PublicIdentity>,
        invite: bool,
    ) -> Result<ConversationId, NodeError> {
        let me = self.identity.public();
        let owner = me.user_id();
        if !members.iter().any(|m| m.user_id() == owner) {
            members.push(me);
        }
        let members = canonical_members(members);
        let channel = crate::channel::new_channel_id();
        let meta = ChannelMeta {
            name: name.to_string(),
            members: members.clone(),
            epoch: 0,
            // The creator is the initial owner. Administration can later transfer
            // through a separately signed control event.
            owner: owner.clone(),
        };

        self.append_event(
            channel,
            EventKind::MembershipChange,
            if invite {
                meta.encode_invitation()
            } else {
                meta.encode()
            },
        )?;
        // Build our own channel state (sets our identity for the sender-key path).
        self.process_channel(channel);
        // Distribute our epoch-0 sender-key distribution to every member.
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
        let sealed = {
            let mut book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state_mut(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            let epoch = state.epoch();
            let skd = state.my_sender_distribution();
            seal_keys_for(&self.identity, members, &skd, epoch)
                .map_err(|e| NodeError::Channel(format!("key sealing failed: {e}")))?
        };
        self.append_event(channel, EventKind::KeyRotation, sealed.encode())?;
        self.persist_my_senders(channel);
        Ok(())
    }

    /// Snapshot `channel`'s `my_sender` (under the channels lock) and persist it to the
    /// durable store so our sending chains survive a restart. Best-effort: a store
    /// write error must not fail an already-appended message. No `MutexGuard` is held
    /// across the persist (the snapshot is taken first, then the lock is dropped).
    pub(in crate::node) fn persist_my_senders(&self, channel: ConversationId) {
        let snapshot = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            book.state(&channel).map(|s| s.export_my_senders())
        };
        if let Some(senders) = snapshot {
            let _ = self
                .channel_senders
                .lock()
                .expect("channel_senders mutex not poisoned")
                .put(&channel, &senders);
        }
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
        let (name, mut new_members, new_epoch, owner) = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            (
                state.name().to_string(),
                state.members().to_vec(),
                state.epoch() + 1,
                state.owner().to_string(),
            )
        };
        // Only the owner may change membership — a non-owner's event would be rejected by
        // every node's `process`, so refuse it locally too (no useless event/epoch bump).
        if self.user_id() != owner {
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
        new_members.push(new_member);
        let new_members = canonical_members(new_members);
        let meta = ChannelMeta {
            name,
            members: new_members.clone(),
            epoch: new_epoch,
            owner,
        };
        self.append_event(
            channel,
            EventKind::MembershipChange,
            meta.encode_invitation(),
        )?;
        self.process_channel(channel);
        self.distribute_channel(channel, &new_members).await;
        Ok(())
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
        let (name, mut new_members, new_epoch, owner) = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            (
                state.name().to_string(),
                state.members().to_vec(),
                state.epoch() + 1,
                state.owner().to_string(),
            )
        };
        // Only the owner may change membership — reject a non-owner's removal locally
        // (every node would reject the resulting event anyway).
        if self.user_id() != owner {
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
        self.distribute_channel(channel, &new_members).await;
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
        let (current_name, members, owner) = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            (
                state.name().to_string(),
                state.members().to_vec(),
                state.owner().to_string(),
            )
        };
        // Only the owner may rename — a non-owner's event would be rejected by every node's
        // `process`, so refuse it locally too.
        if self.user_id() != owner {
            return Err(NodeError::Channel(
                "only the channel owner can rename the channel".into(),
            ));
        }
        let new_name = new_name.trim();
        if new_name.is_empty() {
            return Err(NodeError::InvalidInput(
                "channel name cannot be empty".into(),
            ));
        }
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
        if self.user_id() != owner {
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
        self.distribute_channel(channel, &members).await;
        Ok(())
    }

    /// Publish a self-signed departure, then stop participating locally. The event is
    /// first offered to the old member set while our old group route is still active.
    pub async fn leave_channel(&self, channel: ConversationId) -> Result<(), NodeError> {
        let (owner, members) = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            (state.owner().to_string(), state.members().to_vec())
        };
        let me = self.user_id();
        if me == owner {
            return Err(NodeError::Channel(
                "transfer channel ownership before leaving".into(),
            ));
        }
        if !members.iter().any(|member| member.user_id() == me) {
            return Err(NodeError::Channel("not a channel member".into()));
        }
        self.append_event(channel, EventKind::ChannelLeave, vec![])?;
        self.distribute_channel(channel, &members).await;
        self.process_channel(channel);
        Ok(())
    }

    /// Accept a pending invitation. The signed event makes the decision survive
    /// restart and sync to other copies of this device's log.
    pub async fn accept_channel_invite(&self, channel: ConversationId) -> Result<(), NodeError> {
        let members = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            if !state.pending_invite() || !state.is_member(&self.user_id()) {
                return Err(NodeError::Channel("no pending invitation".into()));
            }
            state.members().to_vec()
        };
        self.append_event(channel, EventKind::ChannelAccept, vec![])?;
        self.process_channel(channel);
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
        self.send_channel_body(channel, MessageBody::new(text.to_vec(), reply_to).encode())
            .await
    }

    /// Send an animated sticker to a channel: `sticker_id` is the bundled sticker's
    /// codepoint id, `fallback` the emoji char shown if a member lacks it.
    pub async fn send_sticker_channel(
        &self,
        channel: ConversationId,
        sticker_id: &str,
        fallback: &[u8],
    ) -> Result<(), NodeError> {
        self.send_channel_body(
            channel,
            MessageBody::sticker(sticker_id.to_string(), fallback.to_vec()).encode(),
        )
        .await
    }

    /// Seal an already-encoded message body with this node's sender key, append it, record
    /// a local plaintext copy, and distribute to channel members.
    async fn send_channel_body(
        &self,
        channel: ConversationId,
        wrapped: Vec<u8>,
    ) -> Result<(), NodeError> {
        // Ensure this node's sender-key distribution for the current epoch is published
        // BEFORE the first seal (which generates+ratchets our chain to n=1). Without
        // this, a non-creator member's messages are undecryptable by everyone.
        let (members, already_distributed) = {
            let mut book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state_mut(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            if !state.is_member(&self.user_id()) || state.pending_invite() {
                return Err(NodeError::Channel("not a channel member".into()));
            }
            let epoch = state.epoch();
            (state.members().to_vec(), state.has_my_sender(epoch))
        };
        if !already_distributed {
            self.distribute_my_sender_key(channel, &members)?;
        }
        let payload = {
            let mut book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state_mut(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            state
                .seal_sender_message(&wrapped)
                .map_err(|e| NodeError::Channel(format!("message sealing failed: {e}")))?
        };
        // Persist our advanced sending chain so a post-restart send resumes THIS chain
        // (receivers are first-wins and keep the chain they already hold).
        self.persist_my_senders(channel);
        let seq = self.append_event(channel, EventKind::Message, payload)?;
        // Keep a local plaintext copy of what we sent — the sender-key wire key is
        // single-use and not self-decryptable, so channel history is served from the
        // stores. Best-effort: a sidecar write error doesn't fail a sealed+appended
        // message.
        let _ = self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .record(channel, seq, now_millis(), &wrapped);
        let event_id = {
            let log = self.log.lock().expect("log mutex not poisoned");
            let author =
                crate::eventlog::event::Author::from_ed25519(self.identity.public().ed25519_pub);
            log.events(&channel)
                .into_iter()
                .find(|event| event.author == author && event.seq == seq)
                .map(|event| event.id)
                .expect("just-appended channel message")
        };
        self.distribute_channel_message(channel, &members, event_id)
            .await;
        Ok(())
    }

    /// Push a control event to every known member, including on large channels.
    /// Membership changes and key rotations must not depend on gossip convergence.
    pub(in crate::node) async fn distribute_channel(
        &self,
        channel: ConversationId,
        members: &[PublicIdentity],
    ) {
        self.distribute_channel_targets(channel, members, None, true)
            .await;
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
            .replicate_to_post_office_targets(
                channel,
                members
                    .iter()
                    .filter(|member| member.user_id() != me)
                    .cloned()
                    .collect(),
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
            self.channel_custody_count(channel, event_id).unwrap_or(0),
            sent_frame_bytes,
            started.elapsed().as_millis(),
        );
    }

    /// Number of current member devices that previously proved durable custody of
    /// one of this device's messages. A historical confirmation is not a guarantee
    /// that the peer still has its copy, and is never a recipient delivery receipt.
    pub fn channel_custody_count(&self, channel: ConversationId, id: EventId) -> Option<usize> {
        let me = crate::eventlog::event::Author::from_ed25519(self.identity.public().ed25519_pub);
        let own_message = self
            .log
            .lock()
            .expect("log mutex not poisoned")
            .get(&id)
            .is_some_and(|event| {
                event.conversation_id == channel
                    && event.author == me
                    && event.kind == EventKind::Message
            });
        if !own_message {
            return None;
        }
        let members: HashSet<_> = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            book.state(&channel)?
                .members()
                .iter()
                .map(|member| member.user_id())
                .filter(|member| book.replica_enabled(channel, member))
                .collect()
        };
        let confirmed = self
            .channel_custody
            .lock()
            .expect("channel_custody mutex not poisoned")
            .peers(id);
        Some(confirmed.intersection(&members).count())
    }

    async fn sync_and_confirm_custody(
        &self,
        channel: ConversationId,
        event_id: EventId,
        peer: &PeerRecord,
    ) -> u64 {
        let Ok(mut conn) = self.privacy_dial(peer.addr, &peer.public).await else {
            return 0;
        };
        let store = self.sync_store(conn.peer_identity());
        if request_round(&mut conn, &store, channel).await.is_ok()
            && matches!(
                request_durable_have(&mut conn, channel, event_id).await,
                Ok(true)
            )
        {
            let _ = self
                .channel_custody
                .lock()
                .expect("channel_custody mutex not poisoned")
                .confirm(event_id, &peer.public.user_id());
        }
        conn.sent_frame_bytes()
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
            return;
        }
        channels.sort_unstable_by_key(|channel| *channel.as_bytes());
        let channel = channels[cursor % channels.len()];
        let previous = after.get(&channel).copied();
        let Some((lamport, event_id)) = self
            .log
            .lock()
            .expect("log mutex not poisoned")
            .message_keys_after(channel, author, previous, 1)
            .into_iter()
            .next()
        else {
            return;
        };
        let cycle = advance_custody_cursor(channel, (lamport, event_id), after, cycles);
        let confirmed = self
            .channel_custody
            .lock()
            .expect("channel_custody mutex not poisoned")
            .peers(event_id);
        let peer = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            book.state(&channel).and_then(|state| {
                let peers: Vec<_> = state
                    .members()
                    .iter()
                    .filter(|member| {
                        confirmed.contains(&member.user_id())
                            && book.replica_enabled(channel, &member.user_id())
                    })
                    .filter_map(|member| {
                        self.routing_peer(&member.user_id())
                            .filter(|peer| peer.public == *member)
                    })
                    .collect();
                select_custody_peer(peers, cycle)
            })
        };
        let Some(peer) = peer else { return };
        let check = async {
            let mut conn = self.privacy_dial(peer.addr, &peer.public).await.ok()?;
            request_durable_have(&mut conn, channel, event_id)
                .await
                .ok()
        };
        if matches!(
            tokio::time::timeout(CHANNEL_RECOVERY_PEER_TIMEOUT, check).await,
            Ok(Some(false))
        ) {
            let _ = self
                .channel_custody
                .lock()
                .expect("channel_custody mutex not poisoned")
                .revoke(event_id, &peer.public.user_id());
        }
    }

    /// Retry one insufficiently replicated own message per tick. The event log is
    /// the durable pending queue, so a restart cannot forget local-only messages.
    async fn retry_one_channel_custody(
        &self,
        cursor: usize,
        after: &mut HashMap<ConversationId, (u64, EventId)>,
        cycles: &mut HashMap<ConversationId, usize>,
    ) {
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
            return;
        }
        channels.sort_unstable_by_key(|(channel, _)| *channel.as_bytes());
        let (channel, members) = &channels[cursor % channels.len()];
        let channel = *channel;
        let author =
            crate::eventlog::event::Author::from_ed25519(self.identity.public().ed25519_pub);
        let candidates: Vec<_> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            log.message_keys_after(
                channel,
                author,
                after.get(&channel).copied(),
                CHANNEL_CUSTODY_SCAN_PER_TICK,
            )
        };
        if candidates.is_empty() {
            return;
        }
        let mut candidates = candidates.into_iter();
        while let Some((event_id, cycle)) =
            next_custody_retry_key(&mut candidates, channel, after, cycles)
        {
            let target = members
                .iter()
                .filter(|member| {
                    member.user_id() != me
                        && self.channel_replica_enabled(channel, &member.user_id())
                })
                .count()
                .min(2);
            if target == 0 || self.channel_custody_count(channel, event_id).unwrap_or(0) >= target {
                continue;
            }
            let confirmed = self
                .channel_custody
                .lock()
                .expect("channel_custody mutex not poisoned")
                .peers(event_id);
            let peers = members
                .iter()
                .filter(|member| member.user_id() != me && !confirmed.contains(&member.user_id()))
                .filter(|member| self.channel_replica_enabled(channel, &member.user_id()))
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
                break;
            }
        }
    }

    async fn distribute_channel_targets(
        &self,
        channel: ConversationId,
        members: &[PublicIdentity],
        fanout: Option<usize>,
        replicate_to_post_office: bool,
    ) {
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
                .conversation_len(&channel)
                .wrapping_add(self.identity.public().ed25519_pub[0] as usize);
            select_channel_targets(peers, members.len(), start, limit)
        } else {
            peers
        };
        // Controls still reach every routed member, but a stalled route must
        // neither block healthy members nor create an unbounded dial burst.
        stream::iter(targets)
            .for_each_concurrent(CHANNEL_CONTROL_FANOUT_CONCURRENCY, |peer| async move {
                let _ = tokio::time::timeout(CHANNEL_RECOVERY_PEER_TIMEOUT, async {
                    if let Ok(mut ch) = self.privacy_dial(peer.addr, &peer.public).await {
                        let store = self.sync_store(ch.peer_identity());
                        let _ = request_round(&mut ch, &store, channel).await;
                    }
                })
                .await;
            })
            .await;
        if replicate_to_post_office {
            let _ = self
                .replicate_to_post_office_targets(
                    channel,
                    members
                        .iter()
                        .filter(|member| member.user_id() != me)
                        .cloned()
                        .collect(),
                )
                .await;
        }
    }

    /// Reconcile one joined channel with an online member. A rotating cursor gives
    /// every channel and member a turn without making the background loop dial the
    /// full group. Any member holding an event can serve it, even if its author is
    /// offline. The ordinary sync store enforces membership and validates events.
    pub(in crate::node) async fn pull_one_channel_from_member(&self, cursor: usize) {
        let me = self.user_id();
        let channels = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let mut channels: Vec<_> = book
                .channel_ids()
                .into_iter()
                .filter_map(|id| {
                    let state = book.state(&id)?;
                    state.is_member(&me).then(|| (id, state.members().to_vec()))
                })
                .collect();
            channels.sort_unstable_by_key(|(id, _)| *id.as_bytes());
            channels
        };
        if channels.is_empty() {
            return;
        }
        let channel_count = channels.len();
        let (channel_id, members) = &channels[cursor % channel_count];
        let peers: Vec<_> = members
            .iter()
            .filter(|member| member.user_id() != me)
            .filter_map(|member| {
                self.routing_peer(&member.user_id())
                    .filter(|peer| peer.public == *member)
            })
            .collect();
        if peers.is_empty() {
            return;
        }
        let start = (cursor / channel_count) % peers.len();
        for offset in 0..peers.len().min(CHANNEL_RECOVERY_PEERS_PER_TICK) {
            let peer = &peers[(start + offset) % peers.len()];
            let result = tokio::time::timeout(CHANNEL_RECOVERY_PEER_TIMEOUT, async {
                let Ok(mut conn) = self.privacy_dial(peer.addr, &peer.public).await else {
                    return None;
                };
                let store = self.sync_store(conn.peer_identity());
                request_round(&mut conn, &store, *channel_id)
                    .await
                    .ok()
                    .map(|report| report.applied)
            })
            .await;
            if let Ok(Some(applied)) = result {
                self.process_channel(*channel_id);
                self.process_file_events(*channel_id);
                // A reachable peer can itself be behind. Try another replica in this
                // tick if it had nothing new, instead of treating connectivity as
                // evidence that this channel is current.
                if applied > 0 {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod fanout_tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::Instant;

    #[test]
    fn large_channel_limits_and_rotates_initial_targets() {
        let peers: Vec<_> = (0..5)
            .map(|port| PeerRecord {
                public: DeviceIdentity::generate().public(),
                addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 4000 + port)),
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
