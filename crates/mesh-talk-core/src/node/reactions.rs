//! Message reactions: react to DM/account/channel messages and aggregate views. Split out of node.rs (one `impl Node` block per domain).

use super::node::now_millis;
use super::*;
use crate::eventlog::event::{ConversationId, Event, EventId, EventKind};
use crate::identity::device::PublicIdentity;
use crate::node::conversation::{account_conversation_id, dm_conversation_id};

impl Node {
    /// Aggregated reactions in the DM with `peer` (derives the conversation id).
    pub fn reactions_dm(&self, peer: &PublicIdentity) -> Vec<ReactionView> {
        let conv = dm_conversation_id(&self.identity.public(), peer);
        self.reactions(conv)
    }

    /// React to a message in a DM (toggle off with `remove = true`).
    pub async fn react_dm(
        &self,
        recipient: &str,
        target: EventId,
        emoji: &str,
        remove: bool,
    ) -> Result<(), NodeError> {
        self.react_dm_if(recipient, target, emoji, remove, |accept| accept())
            .await
    }

    /// Authorize the synchronous durable acceptance before performing network I/O.
    pub async fn react_dm_if(
        &self,
        recipient: &str,
        target: EventId,
        emoji: &str,
        remove: bool,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        let peer = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .get(recipient)
            .cloned()
            .ok_or_else(|| NodeError::UnknownPeer(recipient.to_string()))?;
        let conv = dm_conversation_id(&self.identity.public(), &peer.public);
        let payload = ReactionPayload {
            target,
            emoji: emoji.to_string(),
            remove,
        };
        let sealed = crate::dm::seal(&self.identity, &peer.public.x25519_pub, &payload.encode())
            .map_err(NodeError::Seal)?;
        authorize(&mut || {
            self.append_event(conv, EventKind::React, sealed.clone())?;
            self.record_my_reaction(conv, now_millis(), payload.clone())?;
            Ok(())
        })?;
        self.deliver_direct(&peer, conv).await.ok();
        self.replicate_to_post_office(conv).await.ok();
        Ok(())
    }

    /// React to an account message (toggle off with `remove = true`). `target` is the
    /// logical message id (from account history). Fans the reaction out to every device
    /// of `target_account_id` + our own devices, so it aggregates account-wide.
    pub async fn react_to_account(
        &self,
        target_account_id: &str,
        target: EventId,
        emoji: &str,
        remove: bool,
    ) -> Result<(), NodeError> {
        self.react_to_account_if(target_account_id, target, emoji, remove, |accept| accept())
            .await
    }

    /// Authorize the synchronous durable acceptance before performing network I/O.
    pub async fn react_to_account_if(
        &self,
        target_account_id: &str,
        target: EventId,
        emoji: &str,
        remove: bool,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        let my_account = self.account.account_id();
        let dests = self.account_fanout_targets(target_account_id);
        let envelope = ReactionEnvelope::new(
            my_account.clone(),
            target_account_id.to_string(),
            *target.as_bytes(),
            emoji.to_string(),
            remove,
        )
        .encode();
        // Account reactions retain best-effort device fanout. Reject failed
        // local persistence, but do not stop other destinations on a send failure.
        let acct_conv = account_conversation_id(&my_account, target_account_id);
        let mut deliveries = Vec::new();
        authorize(&mut || {
            let mut failure = None;
            for peer in &dests {
                let sealed =
                    match crate::dm::seal(&self.identity, &peer.public.x25519_pub, &envelope) {
                        Ok(sealed) => sealed,
                        Err(error) => {
                            failure = Some(NodeError::Seal(error));
                            continue;
                        }
                    };
                let conv = dm_conversation_id(&self.identity.public(), &peer.public);
                match self.append_event(conv, EventKind::React, sealed) {
                    Ok(_) => deliveries.push(peer.clone()),
                    Err(error) => failure = Some(error),
                }
            }
            if deliveries.is_empty()
                && let Some(error) = failure
            {
                return Err(error);
            }
            let wall = now_millis();
            let payload = ReactionPayload {
                target,
                emoji: emoji.to_string(),
                remove,
            };
            self.record_my_reaction(acct_conv, wall, payload.clone())?;
            self.publish_own_account_reaction(target_account_id, wall, &payload)
        })?;
        for peer in &deliveries {
            let conv = dm_conversation_id(&self.identity.public(), &peer.public);
            self.deliver_direct(peer, conv).await.ok();
            self.replicate_to_post_office(conv).await.ok();
        }
        Ok(())
    }

    /// Aggregated reactions for the account conversation with `peer_account_id`. Scans
    /// `React` events across every device-pair conversation between us and that
    /// account's devices (and our own devices), opens each account reaction envelope,
    /// keys it by its logical target + reacting account, and merges our own.
    pub fn account_reactions(&self, peer_account_id: &str) -> Vec<ReactionView> {
        let my_account = self.account.account_id();
        let me = self.identity.public().user_id();
        // The device-pair conversations whose React events can carry this account's
        // reactions: ours-with-each-target-device and ours-with-each-own-other-device.
        let convs: Vec<(ConversationId, PublicIdentity, Option<String>)> = {
            self.historical_dm_peers()
                .into_iter()
                .filter(|p| {
                    let account = p.account_id();
                    account.as_deref() == Some(peer_account_id)
                        || (account.as_deref() == Some(my_account.as_str())
                            && p.public().user_id() != me)
                })
                .map(|p| {
                    let public = p.public();
                    (
                        dm_conversation_id(&self.identity.public(), &public),
                        public,
                        p.account_id(),
                    )
                })
                .collect()
        };
        let mut decoded: Vec<(String, u64, ReactionPayload)> =
            self.private_account_reactions(peer_account_id);
        for (conv, author, peer_account) in convs {
            let react_events: Vec<Event> = {
                let log = self.log.lock().expect("log mutex not poisoned");
                log.events(&conv)
                    .into_iter()
                    .filter(|e| e.kind == EventKind::React)
                    .cloned()
                    .collect()
            };
            for event in &react_events {
                if event.author.ed25519_pub() != &author.ed25519_pub {
                    continue;
                }
                let Ok(pt) = crate::dm::open(&self.identity, &author.x25519_pub, &event.ciphertext)
                else {
                    continue;
                };
                if let Some(env) = ReactionEnvelope::decode(&pt) {
                    // Bind the claimed sender account to the peer whose sealed-box key
                    // just authenticated this reaction — no spoofing another account.
                    if peer_account.as_deref() != Some(env.route.sender_account.as_str())
                        || account_conversation_id(
                            &env.route.sender_account,
                            &env.route.recipient_account,
                        ) != account_conversation_id(&my_account, peer_account_id)
                    {
                        continue;
                    }
                    decoded.push((
                        env.route.sender_account,
                        event.wall_clock,
                        ReactionPayload {
                            target: EventId::new(env.target),
                            emoji: env.emoji,
                            remove: env.remove,
                        },
                    ));
                }
            }
        }
        // Merge our own account reactions (sealed to peers; not openable from our log).
        let acct_conv = account_conversation_id(&my_account, peer_account_id);
        for ((c, _, _), (w, p)) in self
            .my_dm_reactions
            .lock()
            .expect("my_dm_reactions mutex not poisoned")
            .iter()
        {
            if *c == acct_conv {
                decoded.push((my_account.clone(), *w, p.clone()));
            }
        }
        aggregate(&decoded)
    }

    /// React to a message in a channel we hold the key for.
    pub async fn react_channel(
        &self,
        channel: ConversationId,
        target: EventId,
        emoji: &str,
        remove: bool,
    ) -> Result<(), NodeError> {
        self.react_channel_if(channel, target, emoji, remove, |accept| accept())
            .await
    }

    /// Authorize the synchronous durable acceptance before performing network I/O.
    pub async fn react_channel_if(
        &self,
        channel: ConversationId,
        target: EventId,
        emoji: &str,
        remove: bool,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        let payload = ReactionPayload {
            target,
            emoji: emoji.to_string(),
            remove,
        };
        let (members, epoch) = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            if !self.channel_identity_is_current(state) {
                return Err(NodeError::Channel("not a channel member".into()));
            }
            (state.recipient_members(), state.epoch())
        };
        // Reactions must be re-readable by every member (aggregated on each `reactions`
        // call), so they are sealed per-member with the DM sealed box, not the
        // single-use sender-key ratchet used for messages.
        let sealed =
            crate::node::channel::SealedPayload::seal(&self.identity, &members, &payload.encode())
                .map_err(|e| NodeError::Channel(format!("reaction seal failed: {e}")))?;
        authorize(&mut || {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            {
                let book = self.channels.lock().expect("channels lock");
                let state = book
                    .state(&channel)
                    .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
                if state.epoch() != epoch || !self.channel_identity_is_current(state) {
                    return Err(NodeError::Channel(
                        "channel membership changed; retry the operation".into(),
                    ));
                }
            }
            self.append_event(channel, EventKind::React, sealed.encode())?;
            Ok(())
        })?;
        self.distribute_channel(channel, &members).await;
        Ok(())
    }

    /// Aggregated reactions for a conversation (DM or channel). Opens each `React`
    /// event with the conversation crypto, merges our own (un-openable) DM reactions,
    /// and folds them per `(target, emoji)`.
    pub fn reactions(&self, conv: ConversationId) -> Vec<ReactionView> {
        let self_uid = self.identity.public().user_id();
        let content_gate = self.channel_content_gate(conv);
        let react_events: Vec<Event> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            log.events(&conv)
                .into_iter()
                .filter(|e| {
                    e.kind == EventKind::React
                        && content_gate.as_ref().is_none_or(|ids| ids.contains(&e.id))
                })
                .cloned()
                .collect()
        };
        let is_channel = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            book.state(&conv).is_some()
        };
        let mut decoded: Vec<(String, u64, ReactionPayload)> = Vec::new();
        for event in &react_events {
            let plaintext = if is_channel {
                let Some(sealed) = crate::node::channel::SealedPayload::decode(&event.ciphertext)
                else {
                    continue;
                };
                let author_uid = event.author.user_id();
                let book = self.channels.lock().expect("channels mutex not poisoned");
                book.state(&conv)
                    .and_then(|s| {
                        s.members()
                            .iter()
                            .find(|m| m.user_id() == author_uid)
                            .map(|m| m.x25519_pub)
                    })
                    .and_then(|author_x| sealed.open(&self.identity, &author_x))
            } else {
                let author_uid = event.author.user_id();
                let sender_x25519 = self
                    .historical_dm_peers()
                    .into_iter()
                    .map(|proof| proof.public())
                    .find(|public| {
                        public.user_id() == author_uid
                            && dm_conversation_id(&self.identity.public(), public) == conv
                    })
                    .map(|public| public.x25519_pub);
                sender_x25519
                    .and_then(|x| crate::dm::open(&self.identity, &x, &event.ciphertext).ok())
            };
            if let Some(p) = plaintext.and_then(|b| ReactionPayload::decode(&b)) {
                decoded.push((event.author.user_id(), event.wall_clock, p));
            }
        }
        // Merge our own DM reactions for this conversation (not in the openable log).
        if !is_channel {
            for ((c, _, _), (w, p)) in self
                .my_dm_reactions
                .lock()
                .expect("my_dm_reactions mutex not poisoned")
                .iter()
            {
                if *c == conv {
                    decoded.push((self_uid.clone(), *w, p.clone()));
                }
            }
        }
        aggregate(&decoded)
    }

    /// Record one of OUR OWN reactions (sealed to the peer, so un-openable from our log)
    /// for later merge in `reactions`/`account_reactions`. Collapsed to the latest action
    /// per `(conv, target, emoji)` — newer wall_clock wins, and on an exact tie a `remove`
    /// beats an `add` — so the map matches `aggregate`'s per-author max-reduction and
    /// stays bounded by the number of distinct (conv, target, emoji) keys (no per-toggle
    /// growth, no linear scan blow-up).
    fn record_my_reaction(
        &self,
        conv: ConversationId,
        wall: u64,
        payload: ReactionPayload,
    ) -> Result<(), NodeError> {
        self.my_dm_reactions
            .lock()
            .expect("my_dm_reactions mutex not poisoned")
            .record(conv, wall, payload)
            .map_err(NodeError::Log)
    }
}

#[cfg(test)]
mod acceptance_tests {
    use super::*;
    use crate::discovery::Announce;
    use crate::identity::{account::Account, device::DeviceIdentity};

    #[tokio::test]
    async fn channel_content_controls_recheck_membership_at_durable_acceptance() {
        for recall in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let peer = DeviceIdentity::generate();
            let proof = Announce::new_with_account(&peer, &Account::generate(), "peer", 1);
            let (node, _, _) = super::super::test_support::node_with_files(
                dir.path(),
                DeviceIdentity::generate(),
                Account::generate(),
                &proof,
            );
            let channel = node.create_channel("group", vec![]).await.unwrap();
            let target = node
                .enqueue_channel_message_reply_expiring_if(
                    channel,
                    b"message",
                    None,
                    None,
                    |commit| commit(),
                )
                .unwrap();
            let authorize = |commit: &mut dyn FnMut() -> Result<(), NodeError>| {
                let metadata = {
                    let book = node.channels.lock().unwrap();
                    let state = book.state(&channel).unwrap();
                    crate::channel::ChannelMeta {
                        name: state.name().into(),
                        members: state.members().to_vec(),
                        epoch: state.epoch() + 1,
                        owner: state.owner().into(),
                    }
                };
                node.append_event(channel, EventKind::MembershipChange, metadata.encode())?;
                node.process_channel(channel);
                commit()
            };
            let result = if recall {
                node.recall_channel_if(channel, target, authorize).await
            } else {
                node.react_channel_if(channel, target, "👍", false, authorize)
                    .await
            };
            assert!(
                matches!(result, Err(NodeError::Channel(message)) if message.contains("membership changed"))
            );
            assert!(
                !node
                    .log
                    .lock()
                    .unwrap()
                    .events(&channel)
                    .iter()
                    .any(|event| { matches!(event.kind, EventKind::React | EventKind::Delete) }),
                "a pre-change sealed control must never be committed after the authority changes"
            );
            assert!(node.recalls.lock().unwrap().targets(&channel).is_empty());
        }
    }

    #[tokio::test]
    async fn rejected_reaction_append_does_not_create_durable_projection() {
        for account_level in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let own = DeviceIdentity::generate();
            let peer = DeviceIdentity::generate();
            let peer_account = Account::generate();
            let proof = Announce::new_with_account(&peer, &peer_account, "peer", 1);
            let (node, _, _) = super::super::test_support::node_with_files(
                dir.path(),
                own,
                Account::generate(),
                &proof,
            );
            let conv = dm_conversation_id(&node.identity.public(), &peer.public());
            node.log
                .lock()
                .unwrap()
                .append(Event::new(
                    &peer,
                    conv,
                    1,
                    vec![],
                    u64::MAX,
                    now_millis(),
                    EventKind::Message,
                    vec![],
                ))
                .unwrap();
            let target = EventId::new([7; 32]);
            let result = if account_level {
                node.react_to_account(&peer_account.account_id(), target, "👍", false)
                    .await
            } else {
                node.react_dm(&peer.user_id(), target, "👍", false).await
            };
            assert!(matches!(
                result,
                Err(NodeError::Log(crate::eventlog::LogError::ClockExhausted))
            ));
            assert_eq!(node.my_dm_reactions.lock().unwrap().len(), 0);
        }
    }
}
