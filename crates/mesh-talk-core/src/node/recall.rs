//! Message recall (unsend): retract a message I sent so it disappears for everyone,
//! within a short window. Recall is ADDITIVE — it appends a `Delete` event that
//! references the target message and propagates exactly like a reaction (DM/account:
//! sealed-box + account fan-out; channel: per-member sealed). It never removes the
//! target from the synced log. At read time `history`/`account_history` collect the
//! recalled ids and render a placeholder. Mirrors `reactions.rs`.
//!
//! Authorization: only the message's own author can recall it. Account recalls bind the
//! claimed sender account to the authenticating peer (like account reactions) and are
//! only honoured when that account is the target message's author; channel recalls are
//! honoured only when the `Delete` event's author equals the target event's author.

use super::node::{Node, now_millis};
use crate::eventlog::LogError;
use crate::eventlog::event::{Author, ConversationId, Event, EventId, EventKind};
use crate::node::NodeError;
use crate::node::conversation::{account_conversation_id, dm_conversation_id};
use crate::node::dm_envelope::RecallEnvelope;
use crate::storage::record_log::EncryptedRecordLog;
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// How long after sending a message it may still be recalled (WeChat's 2 minutes).
pub const RECALL_WINDOW_MS: u64 = 2 * 60 * 1000;

const RECALL_LOG_MAGIC: &[u8; 6] = b"MTRECL";

/// Sticker artwork preserves message recall while ordinary attachments keep
/// their existing contract. Validation prevents a filename alone granting it.
fn recallable_manifest(bytes: &[u8]) -> bool {
    super::files::validated_manifest(bytes).is_some_and(|manifest| {
        manifest.protection().is_some() || crate::file::sticker::is_sticker_name(manifest.name())
    })
}

/// The plaintext payload of a channel/DM `Delete` event: the message being recalled.
/// Sealed like a reaction before it ships.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletePayload {
    pub target: EventId,
}

impl DeletePayload {
    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("delete payload serializes")
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }
}

/// One of OUR OWN recalls, recorded durably. Our recall payloads are sealed to peers, so
/// we can't recover them from our own log after restart — this is how `history` keeps
/// showing our recalled messages as placeholders across restarts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallMark {
    pub conversation: ConversationId,
    pub target: EventId,
    pub wall_clock: u64,
}

/// A local-only, password-encrypted log of our own recalls, indexed by conversation.
pub struct RecallLog {
    file: EncryptedRecordLog<RecallMark>,
    by_conversation: HashMap<ConversationId, Vec<RecallMark>>,
    revision: u64,
}

impl RecallLog {
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, marks) =
            EncryptedRecordLog::<RecallMark>::open(path, password, RECALL_LOG_MAGIC)?;
        let mut by_conversation: HashMap<ConversationId, Vec<RecallMark>> = HashMap::new();
        for mark in marks {
            by_conversation
                .entry(mark.conversation)
                .or_default()
                .push(mark);
        }
        Ok(Self {
            file,
            revision: by_conversation.values().map(Vec::len).sum::<usize>() as u64,
            by_conversation,
        })
    }

    /// Record one of our own recalls (idempotent per `(conversation, target)` — recalling
    /// the same message twice doesn't grow the log).
    pub fn record(
        &mut self,
        conversation: ConversationId,
        target: EventId,
        wall_clock: u64,
    ) -> Result<(), LogError> {
        let already = self
            .by_conversation
            .get(&conversation)
            .is_some_and(|v| v.iter().any(|m| m.target == target));
        if already {
            return Ok(());
        }
        let mark = RecallMark {
            conversation,
            target,
            wall_clock,
        };
        self.file.append_durable(&mark)?;
        self.revision = self.revision.wrapping_add(1);
        self.by_conversation
            .entry(conversation)
            .or_default()
            .push(mark);
        Ok(())
    }

    pub(in crate::node) fn revision(&self) -> u64 {
        self.revision
    }

    /// The targets we ourselves recalled in `conversation`.
    pub fn targets(&self, conversation: &ConversationId) -> Vec<EventId> {
        self.by_conversation
            .get(conversation)
            .map(|v| v.iter().map(|m| m.target).collect())
            .unwrap_or_default()
    }

    /// Own recall tombstones retained for a portable snapshot.
    pub(crate) fn snapshot_marks(&self) -> Vec<RecallMark> {
        let mut marks: Vec<_> = self.by_conversation.values().flatten().cloned().collect();
        marks.sort_by(|a, b| {
            a.conversation
                .as_bytes()
                .cmp(b.conversation.as_bytes())
                .then_with(|| a.target.cmp(&b.target))
        });
        marks
    }
}

#[cfg(test)]
mod recovery_snapshot_tests {
    use super::*;

    #[test]
    fn own_recall_marks_can_be_enumerated_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recalls.log");
        let conversation = ConversationId::new([1; 32]);
        let target = EventId::new([2; 32]);
        RecallLog::open(&path, "pw")
            .unwrap()
            .record(conversation, target, 42)
            .unwrap();
        let reopened = RecallLog::open(&path, "pw").unwrap();
        assert_eq!(
            reopened.snapshot_marks(),
            vec![RecallMark {
                conversation,
                target,
                wall_clock: 42
            }]
        );
    }
}

impl Node {
    /// Recall (unsend) one of OUR OWN account messages: `target` is the logical msg id from
    /// account history. Fails if the message isn't ours or the 2-minute window has passed.
    /// Fans a `RecallEnvelope` (a `Delete` event) out to every device of `target_account_id`
    /// and our own, then records our own recall durably.
    pub async fn recall_account(
        &self,
        target_account_id: &str,
        target: EventId,
    ) -> Result<(), NodeError> {
        self.recall_account_if(target_account_id, target, |commit| commit())
            .await
    }

    /// Enclose signed events and local recall projection in host commit authority.
    pub async fn recall_account_if(
        &self,
        target_account_id: &str,
        target: EventId,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        authorize(&mut || Ok(()))?;
        let my_account = self.account.account_id();
        let acct_conv = account_conversation_id(&my_account, target_account_id);
        // Authorship + window: the message must be ours, i.e. present in our sent sidecar
        // under this account conversation. Its original wall_clock gauges the window.
        let wall = {
            let sent = self.sentlog.lock().expect("sentlog mutex not poisoned");
            sent.entries(&acct_conv)
                .into_iter()
                .find(|e| {
                    super::DmEnvelope::decode(&e.plaintext).is_some_and(|env| {
                        env.msg_id == *target.as_bytes()
                            && env.route.sender_account == my_account
                            && env.route.recipient_account == target_account_id
                    })
                })
                .map(|e| e.wall_clock)
        };
        let wall = wall.or_else(|| {
            let files = self.received_files.lock().expect("received files lock");
            files
                .entries_ref(&acct_conv)
                .iter()
                .find(|row| {
                    row.event_id == target
                        && row.from == self.user_id()
                        && recallable_manifest(&row.plaintext)
                })
                .map(|row| row.wall_clock)
        });
        let Some(wall) = wall else {
            return Err(NodeError::Channel(
                "can only recall your own message".into(),
            ));
        };
        let already_accepted = self
            .recalls
            .lock()
            .expect("recalls mutex")
            .targets(&acct_conv)
            .contains(&target);
        if !already_accepted && now_millis().saturating_sub(wall) > RECALL_WINDOW_MS {
            return Err(NodeError::Channel(
                "the 2-minute recall window has passed".into(),
            ));
        }

        let envelope = RecallEnvelope::new(
            my_account.clone(),
            target_account_id.to_string(),
            *target.as_bytes(),
        )
        .encode();
        let mut deliveries = Vec::new();
        authorize(&mut || {
            for peer in self.account_fanout_targets(target_account_id) {
                let Ok(sealed) =
                    crate::dm::seal(&self.identity, &peer.public.x25519_pub, &envelope)
                else {
                    continue;
                };
                let conv = dm_conversation_id(&self.identity.public(), &peer.public);
                if self.append_event(conv, EventKind::Delete, sealed).is_ok() {
                    deliveries.push((peer, conv));
                }
            }
            // Account recalls retain their best-effort fanout contract, including
            // a durable local recall when every destination is currently unavailable.
            self.record_my_recall(acct_conv, target, wall)?;
            self.publish_own_account_recall(target_account_id, target, wall)
        })?;
        for (peer, conv) in deliveries {
            self.deliver_direct(&peer, conv).await.ok();
            self.replicate_to_post_office(conv).await.ok();
        }
        Ok(())
    }

    /// Recall (unsend) one of OUR OWN channel messages by its event id. Fails if the message
    /// isn't ours / isn't a message / the window has passed. Seals a `DeletePayload`
    /// per-member (like a channel reaction) as a `Delete` event and distributes it.
    pub async fn recall_channel(
        &self,
        channel: ConversationId,
        target: EventId,
    ) -> Result<(), NodeError> {
        self.recall_channel_if(channel, target, |commit| commit())
            .await
    }

    /// Enclose signed events and local recall projection in host commit authority.
    pub async fn recall_channel_if(
        &self,
        channel: ConversationId,
        target: EventId,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        authorize(&mut || Ok(()))?;
        let me = Author::from_ed25519(self.identity.public().ed25519_pub);
        let recallable_file_target = self
            .received_files
            .lock()
            .expect("received files lock")
            .entries_ref(&channel)
            .iter()
            .any(|row| row.event_id == target && recallable_manifest(&row.plaintext));
        let wall = {
            let log = self.log.lock().expect("log mutex not poisoned");
            let event = log
                .get(&target)
                .ok_or_else(|| NodeError::Channel("unknown message".into()))?;
            if event.author != me {
                return Err(NodeError::Channel(
                    "can only recall your own message".into(),
                ));
            }
            if event.kind != EventKind::Message
                && !(event.kind == EventKind::FileManifest && recallable_file_target)
            {
                return Err(NodeError::Channel("not a message".into()));
            }
            event.wall_clock
        };
        if now_millis().saturating_sub(wall) > RECALL_WINDOW_MS {
            return Err(NodeError::Channel(
                "the 2-minute recall window has passed".into(),
            ));
        }

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
        let payload = DeletePayload { target };
        let sealed =
            crate::node::channel::SealedPayload::seal(&self.identity, &members, &payload.encode())
                .map_err(|e| NodeError::Channel(format!("recall seal failed: {e}")))?;
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
            self.append_event(channel, EventKind::Delete, sealed.encode())?;
            self.record_my_recall(channel, target, wall)
        })?;
        self.distribute_channel(channel, &members).await;
        Ok(())
    }

    fn record_my_recall(
        &self,
        conv: ConversationId,
        target: EventId,
        wall_clock: u64,
    ) -> Result<(), NodeError> {
        self.recalls
            .lock()
            .expect("recalls mutex not poisoned")
            .record(conv, target, wall_clock)
            .map_err(NodeError::Log)
    }

    /// The set of recalled message ids for a CHANNEL: our own recalls (durable) plus peer
    /// `Delete` events whose author authored the target message.
    pub(in crate::node) fn recalled_targets_channel(
        &self,
        conv: ConversationId,
    ) -> HashSet<EventId> {
        let content_gate = self.channel_content_gate(conv);
        let mut out: HashSet<EventId> = self
            .recalls
            .lock()
            .expect("recalls mutex not poisoned")
            .targets(&conv)
            .into_iter()
            .collect();

        let delete_events: Vec<Event> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            log.events(&conv)
                .into_iter()
                .filter(|e| {
                    e.kind == EventKind::Delete
                        && content_gate.as_ref().is_none_or(|ids| ids.contains(&e.id))
                })
                .cloned()
                .collect()
        };
        for event in &delete_events {
            let author_uid = event.author.user_id();
            // The recaller's x25519 key (to open their sealed Delete): prefer the channel's
            // current member set, then fall back to the discovery roster. The roster fallback
            // covers a recaller who was removed from the channel within the 2-min recall
            // window but is still a known peer — without it their recall would be dropped and
            // the message would un-tombstone. (The two locks are taken sequentially, never
            // nested, to keep a consistent lock order.) The author's x25519 isn't derivable
            // from the event Author, which carries only the ed25519 key.
            let author_x = {
                let book = self.channels.lock().expect("channels mutex not poisoned");
                book.state(&conv).and_then(|s| {
                    s.members()
                        .iter()
                        .find(|m| m.user_id() == author_uid)
                        .map(|m| m.x25519_pub)
                })
            }
            .or_else(|| {
                self.historical_author(event.author.ed25519_pub())
                    .map(|proof| proof.public().x25519_pub)
            });
            let plaintext = author_x.and_then(|ax| {
                crate::node::channel::SealedPayload::decode(&event.ciphertext)
                    .and_then(|sealed| sealed.open(&self.identity, &ax))
            });
            let Some(p) = plaintext.and_then(|b| DeletePayload::decode(&b)) else {
                continue;
            };
            // Only the message's own author may recall it.
            let target_wall_clock = {
                let log = self.log.lock().expect("log mutex not poisoned");
                log.get(&p.target)
                    .filter(|target| target.author == event.author)
                    .map(|target| target.wall_clock)
            };
            if let Some(wall_clock) = target_wall_clock {
                if let Err(error) = self
                    .recalls
                    .lock()
                    .expect("recalls mutex not poisoned")
                    .record(conv, p.target, wall_clock)
                {
                    log::warn!("could not persist verified channel recall: {error}");
                }
                out.insert(p.target);
            }
        }
        out
    }

    /// The set of recalled (logical) message ids for the ACCOUNT conversation with
    /// `peer_account_id`: our own recalls plus peer `RecallEnvelope`s, each authorised
    /// against the target message's author account.
    pub(in crate::node) fn recalled_targets_account(
        &self,
        peer_account_id: &str,
    ) -> HashSet<EventId> {
        let my_account = self.account.account_id();
        let me = self.identity.public().user_id();
        let acct_conv = account_conversation_id(&my_account, peer_account_id);

        let mut out: HashSet<EventId> = self
            .recalls
            .lock()
            .expect("recalls mutex not poisoned")
            .targets(&acct_conv)
            .into_iter()
            .collect();

        // The device-pair conversations whose Delete events can carry this account's recalls
        // (mirrors `account_reactions`): ours-with-each-target-device and ours-with-each-own-
        // other-device.
        let convs: Vec<(ConversationId, [u8; 32], Option<String>)> = {
            self.historical_dm_peers()
                .into_iter()
                .filter(|p| {
                    let account = p.account_id();
                    let a = account.as_deref();
                    a == Some(peer_account_id)
                        || (a == Some(my_account.as_str()) && p.public().user_id() != me)
                })
                .map(|p| {
                    let public = p.public();
                    (
                        dm_conversation_id(&self.identity.public(), &public),
                        public.x25519_pub,
                        p.account_id(),
                    )
                })
                .collect()
        };
        // Collect the peer recall claims (target, claimed-sender-account) from Delete events,
        // already bound to the authenticating peer key. Decrypting these is the only required
        // work; if there are none we skip the expensive author_of rebuild below.
        let mut claims: Vec<(EventId, String)> = self.private_dm_recall_claims(peer_account_id);
        for (conv, author_x, peer_account) in convs {
            let delete_events: Vec<Event> = {
                let log = self.log.lock().expect("log mutex not poisoned");
                log.events(&conv)
                    .into_iter()
                    .filter(|e| e.kind == EventKind::Delete)
                    .cloned()
                    .collect()
            };
            for event in &delete_events {
                let Ok(pt) = crate::dm::open(&self.identity, &author_x, &event.ciphertext) else {
                    continue;
                };
                let Some(env) = RecallEnvelope::decode(&pt) else {
                    continue;
                };
                // Bind the claimed sender account to the peer key that authenticated it.
                if peer_account.as_deref() != Some(env.route.sender_account.as_str()) {
                    continue;
                }
                claims.push((EventId::new(env.target), env.route.sender_account));
            }
        }
        out = out
            .into_iter()
            .map(|target| self.file_message_logical_id(acct_conv, target))
            .collect();
        for (target, _) in &mut claims {
            *target = self.file_message_logical_id(acct_conv, *target);
        }
        if claims.is_empty() {
            return out;
        }

        // Who authored each logical message in this account conversation (to authorise
        // peer recalls): our sends are ours, received entries carry the sender account.
        let targets: HashSet<_> = claims.iter().map(|(target, _)| *target).collect();
        let mut author_of: HashMap<EventId, (String, u64)> = HashMap::new();
        {
            let sentlog = self.sentlog.lock().expect("sentlog mutex not poisoned");
            let mut entries: Vec<_> = sentlog
                .entries_ref(&acct_conv)
                .iter()
                .filter_map(|entry| {
                    let id = super::DmEnvelope::decode_ref(&entry.plaintext)
                        .map_or(EventId::new([0; 32]), |envelope| {
                            EventId::new(envelope.msg_id)
                        });
                    targets.contains(&id).then_some((id, entry))
                })
                .collect();
            // Keep the original entries() override order, using borrowed rows.
            entries.sort_by_key(|(_, entry)| (entry.wall_clock, entry.seq));
            for (id, entry) in entries {
                author_of.insert(id, (my_account.clone(), entry.wall_clock));
            }
        }
        {
            let received = self.received.lock().expect("received mutex not poisoned");
            let mut entries: Vec<_> = received
                .entries_ref(&acct_conv)
                .iter()
                .filter_map(|entry| {
                    let id = super::DmEnvelope::decode_ref(&entry.plaintext)
                        .map_or(EventId::new([0; 32]), |envelope| {
                            EventId::new(envelope.msg_id)
                        });
                    targets.contains(&id).then_some((id, entry))
                })
                .collect();
            entries.sort_by_key(|(_, entry)| entry.wall_clock);
            for (id, entry) in entries {
                author_of.insert(id, (entry.from.clone(), entry.wall_clock));
            }
        }
        {
            let mut authors: HashMap<String, String> = self
                .historical_dm_peers()
                .into_iter()
                .filter_map(|proof| proof.account_id().map(|account| (proof.user_id, account)))
                .collect();
            authors.insert(self.user_id(), my_account.clone());
            for device in self.linked_devices() {
                authors.insert(device.public.user_id(), my_account.clone());
            }
            let files = self.received_files.lock().expect("received files lock");
            for row in files.entries_ref(&acct_conv) {
                let logical = self.file_message_row_logical_id(row);
                if targets.contains(&logical)
                    && recallable_manifest(&row.plaintext)
                    && let Some(account) = authors.get(&row.from).cloned().or_else(|| {
                        self.file_message_identities
                            .lock()
                            .expect("file identity lock")
                            .row_author_account(row)
                    })
                {
                    author_of.insert(logical, (account, row.wall_clock));
                }
            }
        }
        // Only the message's own author account may recall it.
        for (target, sender_account) in claims {
            if let Some((author, wall_clock)) = author_of.get(&target)
                && author == &sender_account
            {
                // Keep the verified tombstone even after the old device keys and
                // peer roster disappear during recovery.
                if let Err(error) = self
                    .recalls
                    .lock()
                    .expect("recalls mutex not poisoned")
                    .record(acct_conv, target, *wall_clock)
                {
                    log::warn!("could not persist verified peer recall: {error}");
                }
                out.insert(target);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_payload_round_trips_and_rejects_trailing() {
        let p = DeletePayload {
            target: EventId::new([9u8; 32]),
        };
        assert_eq!(DeletePayload::decode(&p.encode()), Some(p.clone()));
        let mut junk = p.encode();
        junk.push(0x01);
        assert_eq!(DeletePayload::decode(&junk), None);
    }

    #[test]
    fn recall_log_records_dedups_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recalls.log");
        let conv = ConversationId::new([1u8; 32]);
        let t1 = EventId::new([1u8; 32]);
        let t2 = EventId::new([2u8; 32]);
        {
            let mut r = RecallLog::open(&path, "pw").unwrap();
            r.record(conv, t1, 1000).unwrap();
            r.record(conv, t1, 1000).unwrap(); // duplicate target → no growth
            r.record(conv, t2, 2000).unwrap();
            let mut targets = r.targets(&conv);
            targets.sort_by_key(|e| *e.as_bytes());
            assert_eq!(targets, vec![t1, t2]);
        }
        // Survives reopen (durable, unlike own reactions).
        let r = RecallLog::open(&path, "pw").unwrap();
        assert_eq!(r.targets(&conv).len(), 2);
        assert!(r.targets(&ConversationId::new([9u8; 32])).is_empty());
    }
}

#[cfg(test)]
mod acceptance_tests {
    use super::*;
    use crate::discovery::Announce;
    use crate::identity::{account::Account, device::DeviceIdentity};

    #[tokio::test]
    async fn rejected_channel_recall_append_does_not_create_tombstone() {
        let dir = tempfile::tempdir().unwrap();
        let own = DeviceIdentity::generate();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "peer", 1);
        let (node, _, _) = super::super::test_support::node_with_files(
            dir.path(),
            own,
            Account::generate(),
            &proof,
        );
        let channel = node.create_channel("local", vec![]).await.unwrap();
        let target = {
            let mut log = node.log.lock().unwrap();
            let (parents, clock) = log.prepare(&channel).unwrap();
            let target = Event::new(
                &node.identity,
                channel,
                100,
                parents,
                clock,
                now_millis(),
                EventKind::Message,
                vec![],
            );
            log.append(target.clone()).unwrap();
            log.append(Event::new(
                &peer,
                channel,
                1,
                vec![target.id],
                u64::MAX,
                now_millis(),
                EventKind::Message,
                vec![],
            ))
            .unwrap();
            target
        };
        assert!(matches!(
            node.recall_channel(channel, target.id).await,
            Err(NodeError::Log(LogError::ClockExhausted))
        ));
        assert!(node.recalls.lock().unwrap().targets(&channel).is_empty());
    }
}
