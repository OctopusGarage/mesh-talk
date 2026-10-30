//! Message reactions: react to DM/account/channel messages and aggregate views. Split out of node.rs (one `impl Node` block per domain).

use super::node::now_millis;
use super::*;
use crate::eventlog::event::{ConversationId, Event, EventId, EventKind};
use crate::identity::device::PublicIdentity;
use crate::node::conversation::{account_conversation_id, dm_conversation_id};

#[cfg(test)]
thread_local! {
    static REACTION_PROJECTION_WORK: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
pub(in crate::node) fn take_reaction_projection_work_for_test() -> (usize, usize) {
    REACTION_PROJECTION_WORK.with(|work| work.replace((0, 0)))
}

impl Node {
    /// Aggregated reactions in the DM with `peer` (derives the conversation id).
    pub fn reactions_dm(&self, peer: &PublicIdentity) -> Result<Vec<ReactionView>, NodeError> {
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
    pub fn account_reactions(&self, peer_account_id: &str) -> Result<Vec<ReactionView>, NodeError> {
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
            self.private_account_reactions(peer_account_id)?;
        for (conv, author, peer_account) in convs {
            let react_events: Vec<Event> = {
                let log = self.log.lock().expect("log mutex not poisoned");
                log.events(&conv)?
                    .into_iter()
                    .filter(|e| e.kind == EventKind::React)
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
        let mut after = None;
        loop {
            let page = self.my_dm_reactions.lock().expect("reactions mutex").page(
                Some(acct_conv),
                None,
                after,
                128,
                8 * 1024 * 1024,
            )?;
            if page.rows.is_empty() {
                break;
            }
            after = page.next;
            for row in page.rows {
                decoded.push((my_account.clone(), row.wall, row.payload));
            }
        }
        for (_, _, payload) in &mut decoded {
            payload.target = self.file_message_logical_id(acct_conv, payload.target)?;
        }
        Ok(aggregate(&decoded))
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
            if !self.channel_identity_is_current(state)? {
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
                if state.epoch() != epoch || !self.channel_identity_is_current(state)? {
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
    pub fn reactions(&self, conv: ConversationId) -> Result<Vec<ReactionView>, NodeError> {
        let content_gate = self.channel_content_gate(conv)?;
        let state = self
            .channels
            .lock()
            .expect("channels mutex not poisoned")
            .state(&conv)
            .map(|state| state.members().to_vec());
        let mut private = Vec::new();
        let mut after = None;
        loop {
            let page = self
                .my_dm_reactions
                .lock()
                .expect("reaction projection mutex")
                .channel_page(conv, after, 128, 8 * 1024 * 1024)?;
            if page.rows.is_empty() {
                break;
            }
            after = page.next;
            private.extend(page.rows);
        }
        let bound: std::collections::HashMap<_, _> = private
            .iter()
            .filter_map(|(source, effect, _, _, account)| {
                account.map(|account| ((*effect, source.ed25519_pub), account))
            })
            .collect();
        // Capture verified directory evidence before the log guard. Current
        // self/sibling identity is never evidence for a historical reaction.
        let proofs = if state.is_some() {
            self.historical_peer_proofs()
        } else {
            self.historical_dm_peers()
        };
        let mut accounts: std::collections::HashMap<_, std::collections::BTreeSet<[u8; 32]>> =
            std::collections::HashMap::new();
        for proof in &proofs {
            if proof.valid_presence()
                && !proof.post_office
                && let Some(certificate) = &proof.account_cert
            {
                accounts
                    .entry((proof.ed25519_pub, proof.x25519_pub))
                    .or_default()
                    .insert(certificate.account_ed25519_pub);
            }
        }
        for patch in self.account_binding_patches() {
            for transition in patch.transitions {
                if transition.verify() {
                    let roots = accounts
                        .entry((transition.public.ed25519_pub, transition.public.x25519_pub))
                        .or_default();
                    roots.insert(transition.previous_certificate.account_ed25519_pub);
                    roots.insert(transition.current_certificate.account_ed25519_pub);
                }
            }
        }
        let ambiguous: std::collections::HashSet<_> = self
            .privacy
            .state
            .read()
            .expect("privacy state mutex")
            .as_ref()
            .map(|state| state.proofs.ambiguous_historical_accounts())
            .unwrap_or_default()
            .into_iter()
            .map(|public| (public.ed25519_pub, public.x25519_pub))
            .collect();
        let account_id =
            |ed25519_pub| crate::identity::account::AccountPublic { ed25519_pub }.account_id();
        let principal = |source: &PublicIdentity, account: Option<String>| {
            account
                .or_else(|| {
                    accounts
                        .get(&(source.ed25519_pub, source.x25519_pub))
                        .filter(|roots| {
                            roots.len() == 1
                                && !ambiguous.contains(&(source.ed25519_pub, source.x25519_pub))
                        })
                        .and_then(|roots| roots.first().copied())
                        .map(account_id)
                })
                .unwrap_or_else(|| source.user_id())
        };
        let events = {
            let log = self.log.lock().expect("log mutex not poisoned");
            let events = log.events(&conv)?;
            let modern = state.is_some()
                && events
                    .iter()
                    .any(|event| event.kind == EventKind::ChannelAccountAuthority);
            events
                .into_iter()
                .filter(|event| {
                    event.kind == EventKind::React
                        && content_gate
                            .as_ref()
                            .is_none_or(|ids| ids.contains(&event.id))
                })
                .map(|event| {
                    // Only modern groups have historical account authority to
                    // replay. Legacy and DM reads stay linear in stored effects.
                    if !modern {
                        return Ok((event.clone(), None, None));
                    }
                    let mut ancestors = std::collections::HashMap::new();
                    let mut pending = event.parents.clone();
                    while let Some(id) = pending.pop() {
                        #[cfg(test)]
                        REACTION_PROJECTION_WORK.with(|work| {
                            let (visits, projections) = work.get();
                            work.set((visits + 1, projections));
                        });
                        if ancestors.contains_key(&id) {
                            continue;
                        }
                        if let Some(parent) = log
                            .get(&id)?
                            .filter(|parent| parent.conversation_id == conv)
                        {
                            pending.extend(parent.parents.iter().copied());
                            ancestors.insert(id, parent);
                        }
                    }
                    #[cfg(test)]
                    REACTION_PROJECTION_WORK.with(|work| {
                        let (visits, projections) = work.get();
                        work.set((visits, projections + 1));
                    });
                    let projection = super::channel::ChannelBook::project_membership(
                        conv,
                        &ancestors.values().collect::<Vec<_>>(),
                    );
                    let source = projection.state.as_ref().and_then(|state| {
                        state
                            .members()
                            .iter()
                            .find(|member| member.ed25519_pub == *event.author.ed25519_pub())
                            .cloned()
                    });
                    let account = if projection.authority.current_event().is_some() {
                        source
                            .as_ref()
                            .and_then(|source| {
                                projection
                                    .state
                                    .as_ref()?
                                    .member_account_id(&source.user_id())
                            })
                            .map(str::to_owned)
                    } else {
                        None
                    };
                    Ok::<_, NodeError>((event.clone(), source, account))
                })
                .collect::<Result<Vec<_>, NodeError>>()?
        };
        let mut decoded = Vec::new();
        for (event, causal_source, causal_account) in events {
            if state.is_some() {
                let source = causal_source
                    .or_else(|| {
                        state
                            .as_ref()?
                            .iter()
                            .find(|member| member.ed25519_pub == *event.author.ed25519_pub())
                            .cloned()
                    })
                    .or_else(|| {
                        proofs
                            .iter()
                            .find(|proof| proof.ed25519_pub == *event.author.ed25519_pub())
                            .map(|proof| proof.public())
                    });
                let Some(source) = source else {
                    continue;
                };
                let plaintext = crate::node::channel::SealedPayload::decode(&event.ciphertext)
                    .and_then(|sealed| sealed.open(&self.identity, &source.x25519_pub));
                if let Some(payload) = plaintext.and_then(|bytes| ReactionPayload::decode(&bytes)) {
                    decoded.push((
                        principal(
                            &source,
                            bound
                                .get(&(event.id, source.ed25519_pub))
                                .copied()
                                .map(account_id)
                                .or(causal_account),
                        ),
                        event.wall_clock,
                        payload,
                    ));
                }
            } else {
                let sender = proofs.iter().map(|proof| proof.public()).find(|source| {
                    source.ed25519_pub == *event.author.ed25519_pub()
                        && dm_conversation_id(&self.identity.public(), source) == conv
                });
                if let Some(payload) = sender
                    .and_then(|source| {
                        crate::dm::open(&self.identity, &source.x25519_pub, &event.ciphertext).ok()
                    })
                    .and_then(|bytes| ReactionPayload::decode(&bytes))
                {
                    decoded.push((event.author.user_id(), event.wall_clock, payload));
                }
            }
        }
        if state.is_none() {
            let mut after = None;
            loop {
                let page = self.my_dm_reactions.lock().expect("reactions mutex").page(
                    Some(conv),
                    None,
                    after,
                    128,
                    8 * 1024 * 1024,
                )?;
                if page.rows.is_empty() {
                    break;
                }
                after = page.next;
                for row in page.rows {
                    decoded.push((self.user_id(), row.wall, row.payload));
                }
            }
        }
        decoded.extend(
            private
                .into_iter()
                .map(|(source, effect, wall, payload, account)| {
                    (
                        principal(
                            &source,
                            account
                                .or_else(|| bound.get(&(effect, source.ed25519_pub)).copied())
                                .map(account_id),
                        ),
                        wall,
                        payload,
                    )
                }),
        );
        Ok(aggregate(&decoded))
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

    #[test]
    fn indexed_authored_reactions_warm_node_reads_no_historical_payloads() {
        let dir = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "peer", 1);
        let open = || {
            super::super::test_support::node_with_files(
                dir.path(),
                DeviceIdentity::from_secret_bytes([91; 32], [90; 32]),
                Account::from_secret_bytes([92; 32]),
                &proof,
            )
            .0
        };
        let node = open();
        let conversation = ConversationId::new([93; 32]);
        for n in 0..64 {
            node.record_my_reaction(
                conversation,
                u64::from(n),
                ReactionPayload {
                    target: EventId::new([n; 32]),
                    emoji: "x".repeat(8192),
                    remove: false,
                },
            )
            .unwrap();
        }
        drop(node);
        drop(open());
        crate::storage::record_log::frame_observations::reset();
        let node = open();
        assert!(
            crate::storage::record_log::frame_observations::maximum_capacity() < 8192,
            "ordinary Node reopen must not read historical reaction frames"
        );
        assert_eq!(node.my_dm_reactions.lock().unwrap().len(), 64);
    }

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
                    .unwrap()
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
