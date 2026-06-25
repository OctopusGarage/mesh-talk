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

use super::node::{now_millis, Node};
use super::queries::decode_account_entry;
use crate::eventlog::event::{Author, ConversationId, Event, EventId, EventKind};
use crate::eventlog::LogError;
use crate::node::conversation::{account_conversation_id, dm_conversation_id};
use crate::node::dm_envelope::RecallEnvelope;
use crate::node::NodeError;
use crate::storage::record_log::EncryptedRecordLog;
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// How long after sending a message it may still be recalled (WeChat's 2 minutes).
pub const RECALL_WINDOW_MS: u64 = 2 * 60 * 1000;

const RECALL_LOG_MAGIC: &[u8; 6] = b"MTRECL";

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
        self.file.append(&mark)?;
        self.by_conversation
            .entry(conversation)
            .or_default()
            .push(mark);
        Ok(())
    }

    /// The targets we ourselves recalled in `conversation`.
    pub fn targets(&self, conversation: &ConversationId) -> Vec<EventId> {
        self.by_conversation
            .get(conversation)
            .map(|v| v.iter().map(|m| m.target).collect())
            .unwrap_or_default()
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
        let my_account = self.account.account_id();
        let acct_conv = account_conversation_id(&my_account, target_account_id);
        // Authorship + window: the message must be ours, i.e. present in our sent sidecar
        // under this account conversation. Its original wall_clock gauges the window.
        let wall = {
            let sent = self.sentlog.lock().expect("sentlog mutex not poisoned");
            sent.entries(&acct_conv)
                .into_iter()
                .find(|e| decode_account_entry(&e.plaintext).0 == target)
                .map(|e| e.wall_clock)
        };
        let Some(wall) = wall else {
            return Err(NodeError::Channel(
                "can only recall your own message".into(),
            ));
        };
        if now_millis().saturating_sub(wall) > RECALL_WINDOW_MS {
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
        for peer in self.account_fanout_targets(target_account_id) {
            let sealed = match crate::dm::seal(&self.identity, &peer.public.x25519_pub, &envelope) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let conv = dm_conversation_id(&self.identity.public(), &peer.public);
            if self.append_event(conv, EventKind::Delete, sealed).is_err() {
                continue;
            }
            self.deliver_direct(&peer, conv).await.ok();
            self.replicate_to_post_office(conv).await.ok();
        }
        self.record_my_recall(acct_conv, target, wall);
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
        let me = Author::from_ed25519(self.identity.public().ed25519_pub);
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
            if event.kind != EventKind::Message {
                return Err(NodeError::Channel("not a message".into()));
            }
            event.wall_clock
        };
        if now_millis().saturating_sub(wall) > RECALL_WINDOW_MS {
            return Err(NodeError::Channel(
                "the 2-minute recall window has passed".into(),
            ));
        }

        let members = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            state.members().to_vec()
        };
        let payload = DeletePayload { target };
        let sealed =
            crate::node::channel::SealedPayload::seal(&self.identity, &members, &payload.encode())
                .map_err(|e| NodeError::Channel(format!("recall seal failed: {e}")))?;
        self.append_event(channel, EventKind::Delete, sealed.encode())?;
        self.distribute_channel(channel, &members).await;
        self.record_my_recall(channel, target, wall);
        Ok(())
    }

    fn record_my_recall(&self, conv: ConversationId, target: EventId, wall_clock: u64) {
        let _ = self
            .recalls
            .lock()
            .expect("recalls mutex not poisoned")
            .record(conv, target, wall_clock);
    }

    /// The set of recalled message ids for a CHANNEL: our own recalls (durable) plus peer
    /// `Delete` events whose author authored the target message.
    pub(in crate::node) fn recalled_targets_channel(
        &self,
        conv: ConversationId,
    ) -> HashSet<EventId> {
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
                .filter(|e| e.kind == EventKind::Delete)
                .cloned()
                .collect()
        };
        for event in &delete_events {
            let author_uid = event.author.user_id();
            let plaintext = {
                let book = self.channels.lock().expect("channels mutex not poisoned");
                crate::node::channel::SealedPayload::decode(&event.ciphertext).and_then(|sealed| {
                    book.state(&conv)
                        .and_then(|s| {
                            s.members()
                                .iter()
                                .find(|m| m.user_id() == author_uid)
                                .map(|m| m.x25519_pub)
                        })
                        .and_then(|author_x| sealed.open(&self.identity, &author_x))
                })
            };
            let Some(p) = plaintext.and_then(|b| DeletePayload::decode(&b)) else {
                continue;
            };
            // Only the message's own author may recall it.
            let authorized = {
                let log = self.log.lock().expect("log mutex not poisoned");
                log.get(&p.target)
                    .map(|t| t.author == event.author)
                    .unwrap_or(false)
            };
            if authorized {
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

        // Who authored each logical message in this account conversation (to authorise
        // peer recalls): our sends are ours, received entries carry the sender account.
        let mut author_of: HashMap<EventId, String> = HashMap::new();
        for e in self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .entries(&acct_conv)
        {
            author_of.insert(decode_account_entry(&e.plaintext).0, my_account.clone());
        }
        for e in self
            .received
            .lock()
            .expect("received mutex not poisoned")
            .entries(&acct_conv)
        {
            author_of.insert(decode_account_entry(&e.plaintext).0, e.from.clone());
        }

        // The device-pair conversations whose Delete events can carry this account's recalls
        // (mirrors `account_reactions`): ours-with-each-target-device and ours-with-each-own-
        // other-device.
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
                let target = EventId::new(env.target);
                // Only the message's own author account may recall it.
                if author_of
                    .get(&target)
                    .is_some_and(|a| a == &env.route.sender_account)
                {
                    out.insert(target);
                }
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
