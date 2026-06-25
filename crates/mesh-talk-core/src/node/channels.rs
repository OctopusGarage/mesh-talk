//! Channel lifecycle: create, membership, sender-key distribution, and sends. Split out of node.rs (one `impl Node` block per domain).

use super::node::now_millis;
use super::*;
use crate::channel::ChannelMeta;
use crate::discovery::roster::PeerRecord;
use crate::eventlog::event::{ConversationId, EventId, EventKind};
use crate::identity::device::PublicIdentity;
use crate::node::channel::seal_keys_for;
use crate::node::session::request_round;
use crate::node::transport::dial;

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

impl Node {
    /// Create a channel named `name` with `members` (the creator is added
    /// automatically). Mints a channel id, posts the membership event, builds our own
    /// channel state, then distributes our epoch-0 sender-key distribution to the
    /// members via a `KeyRotation` event so they can follow our chain.
    pub async fn create_channel(
        &self,
        name: &str,
        mut members: Vec<PublicIdentity>,
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
            // The creator is the channel owner — the only principal allowed to change
            // membership thereafter. Immutable for the life of the channel.
            owner: owner.clone(),
        };

        self.append_event(channel, EventKind::MembershipChange, meta.encode())?;
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
        self.append_event(channel, EventKind::MembershipChange, meta.encode())?;
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
        self.distribute_channel(channel, &members).await;
        Ok(())
    }

    /// Push the channel conversation to each known online member (dial + sync) and
    /// replicate it to the elected post office. Best-effort and fail-soft.
    pub(in crate::node) async fn distribute_channel(
        &self,
        channel: ConversationId,
        members: &[PublicIdentity],
    ) {
        let me = self.identity.public().user_id();
        let targets: Vec<PeerRecord> = {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            members
                .iter()
                .filter(|m| m.user_id() != me)
                .filter_map(|m| roster.get(&m.user_id()).cloned())
                .collect()
        };
        for peer in targets {
            if let Ok(mut ch) = dial(peer.addr, &self.identity, Some(&peer.public)).await {
                let _ = request_round(&mut ch, &self.log, channel).await;
            }
        }
        let _ = self.replicate_to_post_office(channel).await;
    }
}
