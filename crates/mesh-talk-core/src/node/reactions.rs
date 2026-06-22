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
        self.append_event(conv, EventKind::React, sealed)?;
        // Our own DM reaction is sealed to the peer; record it so `reactions` can
        // include it (we can't open it from our own log).
        self.record_my_reaction(conv, now_millis(), payload);
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
        for peer in &dests {
            let sealed = match crate::dm::seal(&self.identity, &peer.public.x25519_pub, &envelope) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let conv = dm_conversation_id(&self.identity.public(), &peer.public);
            if self.append_event(conv, EventKind::React, sealed).is_err() {
                continue;
            }
            self.deliver_direct(peer, conv).await.ok();
            self.replicate_to_post_office(conv).await.ok();
        }
        // Record our own reaction (sealed to peers, so un-openable from our own log)
        // under the account conversation, for `account_reactions` to merge.
        let acct_conv = account_conversation_id(&my_account, target_account_id);
        self.record_my_reaction(
            acct_conv,
            now_millis(),
            ReactionPayload {
                target,
                emoji: emoji.to_string(),
                remove,
            },
        );
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
        let convs: Vec<(ConversationId, [u8; 32], Option<String>)> = {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            roster
                .peers()
                .into_iter()
                .filter(|p| {
                    let a = p.account_id.as_deref();
                    a == Some(peer_account_id)
                        || (a == Some(my_account.as_str()) && p.public.user_id() != me)
                })
                .map(|p| {
                    (
                        dm_conversation_id(&self.identity.public(), &p.public),
                        p.public.x25519_pub,
                        p.account_id.clone(),
                    )
                })
                .collect()
        };
        let mut decoded: Vec<(String, u64, ReactionPayload)> = Vec::new();
        for (conv, author_x, peer_account) in convs {
            let react_events: Vec<Event> = {
                let log = self.log.lock().expect("log mutex not poisoned");
                log.events(&conv)
                    .into_iter()
                    .filter(|e| e.kind == EventKind::React)
                    .cloned()
                    .collect()
            };
            for event in &react_events {
                let Ok(pt) = crate::dm::open(&self.identity, &author_x, &event.ciphertext) else {
                    continue;
                };
                if let Some(env) = ReactionEnvelope::decode(&pt) {
                    // Bind the claimed sender account to the peer whose sealed-box key
                    // just authenticated this reaction — no spoofing another account.
                    if peer_account.as_deref() != Some(env.route.sender_account.as_str()) {
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
        let payload = ReactionPayload {
            target,
            emoji: emoji.to_string(),
            remove,
        };
        let members = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            state.members().to_vec()
        };
        // Reactions must be re-readable by every member (aggregated on each `reactions`
        // call), so they are sealed per-member with the DM sealed box, not the
        // single-use sender-key ratchet used for messages.
        let sealed =
            crate::node::channel::SealedPayload::seal(&self.identity, &members, &payload.encode())
                .map_err(|e| NodeError::Channel(format!("reaction seal failed: {e}")))?;
        self.append_event(channel, EventKind::React, sealed.encode())?;
        self.distribute_channel(channel, &members).await;
        Ok(())
    }

    /// Aggregated reactions for a conversation (DM or channel). Opens each `React`
    /// event with the conversation crypto, merges our own (un-openable) DM reactions,
    /// and folds them per `(target, emoji)`.
    pub fn reactions(&self, conv: ConversationId) -> Vec<ReactionView> {
        let self_uid = self.identity.public().user_id();
        let react_events: Vec<Event> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            log.events(&conv)
                .into_iter()
                .filter(|e| e.kind == EventKind::React)
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
                let sender_x25519 = {
                    let roster = self.roster.lock().expect("roster mutex not poisoned");
                    roster.get(&author_uid).map(|p| p.public.x25519_pub)
                };
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
    fn record_my_reaction(&self, conv: ConversationId, wall: u64, payload: ReactionPayload) {
        let key = (conv, payload.target, payload.emoji.clone());
        let mut map = self
            .my_dm_reactions
            .lock()
            .expect("my_dm_reactions mutex not poisoned");
        match map.get(&key) {
            // Newer wins; on an exact wall-clock tie a remove wins (matches `aggregate`).
            Some((cur_wall, _)) if wall > *cur_wall || (wall == *cur_wall && payload.remove) => {
                map.insert(key, (wall, payload));
            }
            Some(_) => {}
            None => {
                map.insert(key, (wall, payload));
            }
        }
    }
}
