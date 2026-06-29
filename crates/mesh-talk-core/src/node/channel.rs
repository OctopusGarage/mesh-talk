//! Channel orchestration over the event log: the per-member sealed
//! sender-key-distribution payload carried by `KeyRotation` events, the payload
//! builders for the three channel event kinds, and a [`ChannelBook`] that replays a
//! channel's events into per-channel state + decrypted messages. Operates on
//! `Event`s — no live network.

use crate::channel::sender_key::SenderKeyDistribution;
use crate::channel::{ChannelMeta, ChannelState};
use crate::eventlog::event::{Author, ConversationId, Event, EventId, EventKind};
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::node::message::MessageBody;
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// The `KeyRotation` event payload: the author's epoch sender-key distribution
/// sealed to each member. `sender` is the author's user id (whose chain the SKD
/// seeds); each `(user_id, sealed)` is `dm::seal(author, member.x25519, skd_bytes)`,
/// so only that member can open its entry. The post office relays it but never reads
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedKeys {
    pub epoch: u64,
    pub sender: String,
    pub entries: Vec<SealedKeyEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedKeyEntry {
    pub user_id: String,
    pub sealed: Vec<u8>,
}

impl SealedKeys {
    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("sealed keys serialize")
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }
}

/// Seal `author`'s epoch sender-key distribution to each member's X25519, producing
/// the `KeyRotation` payload. Members open their entry to follow `author`'s chain
/// from `n = 0`. `author` is the sender whose static key the recipients use to open.
pub fn seal_keys_for(
    author: &DeviceIdentity,
    members: &[PublicIdentity],
    skd: &SenderKeyDistribution,
    epoch: u64,
) -> Result<SealedKeys, crate::channel::ChannelError> {
    let skd_bytes = skd.encode();
    let mut entries = Vec::with_capacity(members.len());
    for member in members {
        let sealed = crate::dm::seal(author, &member.x25519_pub, &skd_bytes)
            .map_err(crate::channel::ChannelError::Seal)?;
        entries.push(SealedKeyEntry {
            user_id: member.user_id(),
            sealed,
        });
    }
    Ok(SealedKeys {
        epoch,
        sender: author.public().user_id(),
        entries,
    })
}

/// Open the sender-key distribution sealed to `me` in a `SealedKeys`, using the
/// rotation author's X25519 (looked up from the channel membership). `None` if there
/// is no entry for us or it fails to open/decode.
fn open_my_skd(
    me: &DeviceIdentity,
    author_x25519: &[u8; 32],
    sealed: &SealedKeys,
) -> Option<SenderKeyDistribution> {
    let my_uid = me.public().user_id();
    let entry = sealed.entries.iter().find(|e| e.user_id == my_uid)?;
    let bytes = crate::dm::open(me, author_x25519, &entry.sealed).ok()?;
    SenderKeyDistribution::decode(&bytes)
}

/// A channel payload (e.g. a reaction) sealed per-member with the DM sealed box: the
/// author's bytes sealed to each member's X25519, so any member can open their entry.
/// Used for content that must be RE-READABLE by all members (no forward secrecy) —
/// unlike channel messages, which use single-use sender keys. The post office relays
/// it but never reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedPayload {
    pub sender: String,
    pub entries: Vec<SealedKeyEntry>,
}

impl SealedPayload {
    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("sealed payload serialize")
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }

    /// Seal `plaintext` to each member's X25519. `author` is the sender whose static
    /// key the recipients use to open.
    pub fn seal(
        author: &DeviceIdentity,
        members: &[PublicIdentity],
        plaintext: &[u8],
    ) -> Result<Self, crate::channel::ChannelError> {
        let mut entries = Vec::with_capacity(members.len());
        for member in members {
            let sealed = crate::dm::seal(author, &member.x25519_pub, plaintext)
                .map_err(crate::channel::ChannelError::Seal)?;
            entries.push(SealedKeyEntry {
                user_id: member.user_id(),
                sealed,
            });
        }
        Ok(SealedPayload {
            sender: author.public().user_id(),
            entries,
        })
    }

    /// Open the bytes sealed to `me`, using the author's X25519. `None` if there is no
    /// entry for us or it fails to open.
    pub fn open(&self, me: &DeviceIdentity, author_x25519: &[u8; 32]) -> Option<Vec<u8>> {
        let my_uid = me.public().user_id();
        let entry = self.entries.iter().find(|e| e.user_id == my_uid)?;
        crate::dm::open(me, author_x25519, &entry.sealed).ok()
    }
}

/// A decrypted channel message surfaced to the application. Carries the source
/// event's id + wall-clock and the decoded plaintext body so the node can persist it
/// to the received store (sender-key wire keys are single-use, so history is served
/// from the store, not by re-opening the event).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedChannelMessage {
    pub channel_id: ConversationId,
    pub channel_name: String,
    pub from: String,
    pub text: Vec<u8>,
    pub reply_to: Option<EventId>,
    pub event_id: EventId,
    pub wall_clock: u64,
    /// The wrapped `MessageBody` plaintext (text + reply_to), for the received store.
    pub wrapped: Vec<u8>,
}

/// A node's view of all the channels it knows: each channel's [`ChannelState`]
/// keyed by channel id. [`ChannelBook::process`] replays a channel's events (in the
/// causal order the log returns them) to update membership/keys and surface
/// decrypted messages.
#[derive(Default)]
pub struct ChannelBook {
    states: HashMap<ConversationId, ChannelState>,
    emitted: HashSet<EventId>,
}

impl ChannelBook {
    pub fn new() -> Self {
        ChannelBook::default()
    }

    /// The state for a known channel, if any.
    pub fn state(&self, channel_id: &ConversationId) -> Option<&ChannelState> {
        self.states.get(channel_id)
    }

    /// Mutable state for a known channel (needed to seal/open with sender keys, which
    /// ratchet the chain).
    pub fn state_mut(&mut self, channel_id: &ConversationId) -> Option<&mut ChannelState> {
        self.states.get_mut(channel_id)
    }

    /// The ids of all channels this book knows.
    pub fn channel_ids(&self) -> Vec<crate::eventlog::event::ConversationId> {
        self.states.keys().copied().collect()
    }

    /// Replay `events` (a channel's events, in log/causal order) for the node `me`.
    /// Updates membership + keys and returns the channel `Message`s that are newly
    /// decryptable and authored by someone other than `me`. Events we can't act on
    /// (a key we don't have, a membership we're not in) are skipped.
    ///
    /// Call with the channel's FULL event log, not a delta: an event skipped because
    /// its prerequisite hasn't been applied yet (e.g. a `KeyRotation` seen before its
    /// `MembershipChange` under out-of-order delivery) is recovered on the next
    /// full-log replay, where the prerequisite is processed first. The caller dedups
    /// the returned messages across calls (the live node uses an emitted-id set).
    pub fn process(
        &mut self,
        me: &DeviceIdentity,
        channel_id: ConversationId,
        events: &[&Event],
    ) -> Vec<ReceivedChannelMessage> {
        let my_author = Author::from_ed25519(me.public().ed25519_pub);
        let mut out = Vec::new();

        for event in events {
            match event.kind {
                EventKind::MembershipChange => {
                    if let Some(meta) = ChannelMeta::decode(&event.ciphertext) {
                        let author_uid = event.author.user_id();
                        match self.states.get_mut(&channel_id) {
                            Some(state) => {
                                // Authorization: ONLY THE OWNER may change an established
                                // channel's membership. The event author must equal the
                                // channel's (immutable, genesis) owner; a non-owner member's
                                // add/remove is dropped by every node (not merely hidden in the
                                // UI). `apply_meta` additionally pins the owner, so an
                                // owner-authored snapshot that renames the owner is rejected
                                // too — no ownership hijack via a higher epoch.
                                if author_uid == state.owner() {
                                    state.apply_meta(meta);
                                }
                            }
                            None => {
                                // First time we hear of this channel: an invite. Trust-on-first-
                                // use — accept only if it includes US, the author is the owner
                                // the snapshot declares, and that owner is itself a member. This
                                // genesis snapshot pins the owner for all later changes.
                                if meta.is_member(&me.public().user_id())
                                    && author_uid == meta.owner
                                    && meta.is_member(&meta.owner)
                                {
                                    let mut state = ChannelState::from_meta(channel_id, meta);
                                    state.set_identity(me.public().user_id());
                                    self.states.insert(channel_id, state);
                                }
                            }
                        }
                    }
                }
                EventKind::ChannelRename => {
                    // Owner-only, like membership: a non-owner's rename is dropped by every
                    // node. Applied last-writer-wins by (lamport, id), independent of the
                    // membership epoch.
                    if let Some(state) = self.states.get_mut(&channel_id) {
                        if event.author.user_id() == state.owner() {
                            if let Ok(name) = std::str::from_utf8(&event.ciphertext) {
                                state.apply_rename(name, event.lamport, event.id);
                            }
                        }
                    }
                }
                EventKind::KeyRotation => {
                    let Some(sealed) = SealedKeys::decode(&event.ciphertext) else {
                        continue;
                    };
                    let Some(state) = self.states.get_mut(&channel_id) else {
                        continue;
                    };
                    let author_uid = event.author.user_id();
                    let Some(author_x25519) = state
                        .members()
                        .iter()
                        .find(|m| m.user_id() == author_uid)
                        .map(|m| m.x25519_pub)
                    else {
                        continue;
                    };
                    if let Some(skd) = open_my_skd(me, &author_x25519, &sealed) {
                        state.record_sender_chain(author_uid, sealed.epoch, &skd);
                    }
                }
                EventKind::Message => {
                    if event.author == my_author || self.emitted.contains(&event.id) {
                        continue; // our own message, or already surfaced
                    }
                    let Some(state) = self.states.get_mut(&channel_id) else {
                        continue;
                    };
                    if let Some(plaintext) =
                        state.open_sender_message(&event.author.user_id(), &event.ciphertext)
                    {
                        let body = MessageBody::decode(&plaintext);
                        let msg = ReceivedChannelMessage {
                            channel_id,
                            channel_name: state.name().to_string(),
                            from: event.author.user_id(),
                            text: body.text,
                            reply_to: body.reply_to,
                            event_id: event.id,
                            wall_clock: event.wall_clock,
                            wrapped: plaintext,
                        };
                        self.emitted.insert(event.id);
                        out.push(msg);
                    }
                }
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{new_channel_id, ChannelMeta, ChannelState};
    use crate::eventlog::event::{Event, EventKind};
    use crate::eventlog::store::EventLog;
    use crate::eventlog::sync::reconcile;

    fn append(
        log: &mut EventLog,
        author: &DeviceIdentity,
        channel: ConversationId,
        kind: EventKind,
        ciphertext: Vec<u8>,
    ) {
        let (parents, lamport) = log.prepare(&channel);
        let self_author = Author::from_ed25519(author.public().ed25519_pub);
        let seq = log
            .version_vector(&channel)
            .get(&self_author)
            .copied()
            .unwrap_or(0)
            + 1;
        let event = Event::new(author, channel, seq, parents, lamport, 0, kind, ciphertext);
        log.append(event).unwrap();
    }

    fn collect<'a>(log: &'a EventLog, channel: &ConversationId) -> Vec<&'a Event> {
        log.events(channel)
    }

    /// Build the author's own [`ChannelState`] for `meta`, with its identity set and
    /// a fresh epoch sender key generated (so it can seal). Returns the state.
    fn author_state(
        author: &DeviceIdentity,
        channel: ConversationId,
        meta: &ChannelMeta,
    ) -> ChannelState {
        let mut state = ChannelState::from_meta(channel, meta.clone());
        state.set_identity(author.public().user_id());
        state
    }

    #[test]
    fn sealed_keys_round_trip_and_reject_trailing() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut alice_state = author_state(
            &alice,
            new_channel_id(),
            &ChannelMeta {
                name: "general".into(),
                members: vec![alice.public(), bob.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            },
        );
        let skd = alice_state.my_sender_distribution();
        let sealed = seal_keys_for(&alice, &[alice.public(), bob.public()], &skd, 0).unwrap();

        let bytes = sealed.encode();
        assert_eq!(SealedKeys::decode(&bytes), Some(sealed.clone()));
        let mut junk = bytes.clone();
        junk.push(0xAB);
        assert_eq!(SealedKeys::decode(&junk), None);
        assert_eq!(sealed.entries.len(), 2);
        assert_eq!(sealed.sender, alice.public().user_id());
    }

    #[test]
    fn a_member_opens_its_own_sealed_skd() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut alice_state = author_state(
            &alice,
            new_channel_id(),
            &ChannelMeta {
                name: "general".into(),
                members: vec![alice.public(), bob.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            },
        );
        let skd = alice_state.my_sender_distribution();
        let sealed = seal_keys_for(&alice, &[alice.public(), bob.public()], &skd, 0).unwrap();

        let opened = open_my_skd(&bob, &alice.public().x25519_pub, &sealed).unwrap();
        assert_eq!(opened.encode(), skd.encode());
    }

    #[test]
    fn a_non_member_has_no_entry_to_open() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let mut alice_state = author_state(
            &alice,
            new_channel_id(),
            &ChannelMeta {
                name: "general".into(),
                members: vec![alice.public(), bob.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            },
        );
        let skd = alice_state.my_sender_distribution();
        let sealed = seal_keys_for(&alice, &[alice.public(), bob.public()], &skd, 0).unwrap();
        assert!(open_my_skd(&carol, &alice.public().x25519_pub, &sealed).is_none());
    }

    #[test]
    fn channel_create_message_and_rotation_on_add() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let channel = new_channel_id();

        // Alice creates the channel {Alice, Bob} at epoch 0.
        let mut alice_log = EventLog::default();
        let meta0 = ChannelMeta {
            name: "general".into(),
            members: vec![alice.public(), bob.public()],
            epoch: 0,
            owner: alice.public().user_id(),
        };
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta0.encode(),
        );
        let mut alice_state = author_state(&alice, channel, &meta0);
        let skd0 = alice_state.my_sender_distribution();
        let sealed0 = seal_keys_for(&alice, &meta0.members, &skd0, 0).unwrap();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed0.encode(),
        );
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::Message,
            alice_state.seal_sender_message(b"hello team").unwrap(),
        );

        // Bob reconciles + processes → learns the channel, records Alice's chain, reads it.
        let mut bob_log = EventLog::default();
        reconcile(&mut bob_log, &mut alice_log, channel);
        let mut bob_book = ChannelBook::new();
        let got = bob_book.process(&bob, channel, &collect(&bob_log, &channel));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, b"hello team");
        assert_eq!(got[0].from, alice.public().user_id());
        assert_eq!(bob_book.state(&channel).unwrap().epoch(), 0);

        // Alice adds Carol → epoch 1, re-distributing her epoch-1 SKD.
        let meta1 = ChannelMeta {
            name: "general".into(),
            members: vec![alice.public(), bob.public(), carol.public()],
            epoch: 1,
            owner: alice.public().user_id(),
        };
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta1.encode(),
        );
        alice_state.apply_meta(meta1.clone());
        let skd1 = alice_state.my_sender_distribution();
        let sealed1 = seal_keys_for(&alice, &meta1.members, &skd1, 1).unwrap();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed1.encode(),
        );
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::Message,
            alice_state.seal_sender_message(b"welcome carol").unwrap(),
        );

        // Carol reconciles fresh → gets epoch-1 SKD only → reads "welcome carol".
        let mut carol_log = EventLog::default();
        reconcile(&mut carol_log, &mut alice_log, channel);
        let mut carol_book = ChannelBook::new();
        let carol_got = carol_book.process(&carol, channel, &collect(&carol_log, &channel));
        assert!(carol_got.iter().any(|m| m.text == b"welcome carol"));
        assert_eq!(carol_book.state(&channel).unwrap().epoch(), 1);

        // Bob reconciles the new events and reads the epoch-1 message.
        reconcile(&mut bob_log, &mut alice_log, channel);
        let bob_got2 = bob_book.process(&bob, channel, &collect(&bob_log, &channel));
        assert!(bob_got2.iter().any(|m| m.text == b"welcome carol"));
        assert_eq!(bob_book.state(&channel).unwrap().epoch(), 1);
    }

    #[test]
    fn late_joiner_cannot_read_pre_join_history() {
        // Carol is added at epoch 1. She must read epoch-1 messages but NOT the epoch-0
        // message sent before she joined (she never receives an epoch-0 sender chain).
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let channel = new_channel_id();
        let mut alice_log = EventLog::default();

        let meta0 = ChannelMeta {
            name: "general".into(),
            members: vec![alice.public(), bob.public()],
            epoch: 0,
            owner: alice.public().user_id(),
        };
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta0.encode(),
        );
        let mut alice_state = author_state(&alice, channel, &meta0);
        let skd0 = alice_state.my_sender_distribution();
        let sealed0 = seal_keys_for(&alice, &meta0.members, &skd0, 0).unwrap();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed0.encode(),
        );
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::Message,
            alice_state.seal_sender_message(b"secret pre-join").unwrap(),
        );

        // Add Carol at epoch 1 and send a post-join message.
        let meta1 = ChannelMeta {
            name: "general".into(),
            members: vec![alice.public(), bob.public(), carol.public()],
            epoch: 1,
            owner: alice.public().user_id(),
        };
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta1.encode(),
        );
        alice_state.apply_meta(meta1.clone());
        let skd1 = alice_state.my_sender_distribution();
        let sealed1 = seal_keys_for(&alice, &meta1.members, &skd1, 1).unwrap();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed1.encode(),
        );
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::Message,
            alice_state.seal_sender_message(b"post-join hello").unwrap(),
        );

        // Carol syncs the FULL log (incl. the epoch-0 message) and processes it.
        let mut carol_log = EventLog::default();
        reconcile(&mut carol_log, &mut alice_log, channel);
        let mut carol_book = ChannelBook::new();
        let got = carol_book.process(&carol, channel, &collect(&carol_log, &channel));
        let texts: Vec<&[u8]> = got.iter().map(|m| m.text.as_slice()).collect();
        assert!(
            texts.contains(&&b"post-join hello"[..]),
            "must read post-join"
        );
        assert!(
            !texts.contains(&&b"secret pre-join"[..]),
            "late joiner must NOT read pre-join history",
        );
    }

    #[test]
    fn removed_member_cannot_read_post_removal_messages() {
        // Group forward security: after Bob is removed (epoch bump + a re-key that
        // excludes him), Bob cannot decrypt messages sent in the new epoch.
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let channel = new_channel_id();
        let mut alice_log = EventLog::default();

        let meta0 = ChannelMeta {
            name: "general".into(),
            members: vec![alice.public(), bob.public()],
            epoch: 0,
            owner: alice.public().user_id(),
        };
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta0.encode(),
        );
        let mut alice_state = author_state(&alice, channel, &meta0);
        let skd0 = alice_state.my_sender_distribution();
        let sealed0 = seal_keys_for(&alice, &meta0.members, &skd0, 0).unwrap();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed0.encode(),
        );
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::Message,
            alice_state.seal_sender_message(b"before removal").unwrap(),
        );

        // Bob is up to date and reads the epoch-0 message.
        let mut bob_log = EventLog::default();
        reconcile(&mut bob_log, &mut alice_log, channel);
        let mut bob_book = ChannelBook::new();
        assert!(bob_book
            .process(&bob, channel, &collect(&bob_log, &channel))
            .iter()
            .any(|m| m.text == b"before removal"));

        // Alice removes Bob → epoch 1, re-keying ONLY to the remaining member (herself).
        let meta1 = ChannelMeta {
            name: "general".into(),
            members: vec![alice.public()],
            epoch: 1,
            owner: alice.public().user_id(),
        };
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta1.encode(),
        );
        alice_state.apply_meta(meta1.clone());
        let skd1 = alice_state.my_sender_distribution();
        let sealed1 = seal_keys_for(&alice, &meta1.members, &skd1, 1).unwrap();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed1.encode(),
        );
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::Message,
            alice_state.seal_sender_message(b"after removal").unwrap(),
        );

        // Bob syncs everything and processes — he must NOT be able to read the epoch-1
        // message (the re-key excluded him; he holds no epoch-1 chain).
        reconcile(&mut bob_log, &mut alice_log, channel);
        let got = bob_book.process(&bob, channel, &collect(&bob_log, &channel));
        assert!(
            !got.iter().any(|m| m.text == b"after removal"),
            "removed member must NOT read post-removal messages",
        );
    }

    #[test]
    fn process_does_not_re_emit_an_already_seen_message() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let channel = new_channel_id();
        let mut alice_log = EventLog::default();
        let meta0 = ChannelMeta {
            name: "general".into(),
            members: vec![alice.public(), bob.public()],
            epoch: 0,
            owner: alice.public().user_id(),
        };
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta0.encode(),
        );
        let mut alice_state = author_state(&alice, channel, &meta0);
        let skd0 = alice_state.my_sender_distribution();
        let sealed0 = seal_keys_for(&alice, &meta0.members, &skd0, 0).unwrap();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed0.encode(),
        );
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::Message,
            alice_state.seal_sender_message(b"hi").unwrap(),
        );

        let mut book = ChannelBook::new();
        let first = book.process(&bob, channel, &collect(&alice_log, &channel));
        assert_eq!(first.len(), 1);
        // Re-feeding the same events does NOT re-open the message (single-use key;
        // the emitted set + consumed chain key both prevent a second surface).
        let second = book.process(&bob, channel, &collect(&alice_log, &channel));
        assert!(second.is_empty());
    }

    #[test]
    fn a_non_owner_cannot_change_channel_membership() {
        // Alice (owner) + Bob in a channel. Mallory is NOT a member but knows the channel
        // id; she forges a higher-epoch MembershipChange adding herself, naming Alice as
        // owner so the snapshot looks legitimate. A current member (Bob) must REJECT it —
        // only the OWNER (the event author == owner) may change membership.
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mallory = DeviceIdentity::generate();
        let channel = new_channel_id();

        let mut log = EventLog::default();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "general".into(),
                members: vec![alice.public(), bob.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            }
            .encode(),
        );
        // Mallory forges {Alice, Bob, Mallory} at a higher epoch, signed by HERSELF but
        // claiming Alice as owner. The author (Mallory) != owner (Alice) → rejected.
        append(
            &mut log,
            &mallory,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "general".into(),
                members: vec![alice.public(), bob.public(), mallory.public()],
                epoch: 1,
                owner: alice.public().user_id(),
            }
            .encode(),
        );

        let mut bob_book = ChannelBook::new();
        bob_book.process(&bob, channel, &collect(&log, &channel));
        let state = bob_book.state(&channel).unwrap();
        assert_eq!(
            state.epoch(),
            0,
            "forged change by a non-owner must be ignored"
        );
        assert!(
            !state
                .members()
                .iter()
                .any(|m| m.user_id() == mallory.public().user_id()),
            "a non-owner cannot add a member to the channel"
        );
    }

    #[test]
    fn a_member_who_is_not_owner_cannot_kick_anyone() {
        // The core fix: Alice owns a channel {Alice, Bob, Carol}. Bob (a member but NOT the
        // owner) authors a higher-epoch MembershipChange that removes Carol. Every node must
        // IGNORE it — Carol stays a member on Alice's, Carol's, and a fresh node's view.
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let channel = new_channel_id();

        let mut log = EventLog::default();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "g".into(),
                members: vec![alice.public(), bob.public(), carol.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            }
            .encode(),
        );
        // Bob (not the owner) tries to kick Carol at a higher epoch, naming Alice as owner.
        append(
            &mut log,
            &bob,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "g".into(),
                members: vec![alice.public(), bob.public()],
                epoch: 1,
                owner: alice.public().user_id(),
            }
            .encode(),
        );

        for viewer in [&alice, &bob, &carol] {
            let mut book = ChannelBook::new();
            book.process(viewer, channel, &collect(&log, &channel));
            let state = book.state(&channel).unwrap();
            assert_eq!(state.epoch(), 0, "non-owner kick must be ignored");
            assert!(
                state
                    .members()
                    .iter()
                    .any(|m| m.user_id() == carol.public().user_id()),
                "the victim must remain a member on every node",
            );
        }
    }

    #[test]
    fn ownership_cannot_be_hijacked_by_a_higher_epoch() {
        // Mallory (a member) tries to seize ownership with a higher-epoch snapshot that
        // names HERSELF owner (and is authored by her). The owner is pinned at genesis, so
        // the snapshot — author == its claimed owner but != the genesis owner — is rejected.
        let alice = DeviceIdentity::generate();
        let mallory = DeviceIdentity::generate();
        let channel = new_channel_id();

        let mut log = EventLog::default();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "g".into(),
                members: vec![alice.public(), mallory.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            }
            .encode(),
        );
        append(
            &mut log,
            &mallory,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "g".into(),
                members: vec![mallory.public()],
                epoch: 9,
                owner: mallory.public().user_id(),
            }
            .encode(),
        );

        let mut book = ChannelBook::new();
        book.process(&alice, channel, &collect(&log, &channel));
        let state = book.state(&channel).unwrap();
        assert_eq!(
            state.epoch(),
            0,
            "ownership-hijack snapshot must be ignored"
        );
        assert_eq!(state.owner(), alice.public().user_id());
        assert!(state
            .members()
            .iter()
            .any(|m| m.user_id() == alice.public().user_id()));
    }

    #[test]
    fn a_node_excluded_by_the_winning_membership_change_still_converges() {
        // epoch 0: {alice, bob, carol}, owner alice. Two same-epoch-1 changes BOTH authored
        // by the owner (e.g. concurrent edits from the owner's two devices) — {alice, carol}
        // (drops bob) and {alice, bob} (drops carol). The deterministic tie-break picks one
        // winner; BOTH bob and carol must converge to it, even the one the winner excludes
        // (the regression: an excluded node used to skip + keep a stale view).
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let channel = new_channel_id();

        let mut log = EventLog::default();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "g".into(),
                members: vec![alice.public(), bob.public(), carol.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            }
            .encode(),
        );
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "g".into(),
                members: vec![alice.public(), carol.public()],
                epoch: 1,
                owner: alice.public().user_id(),
            }
            .encode(),
        );
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "g".into(),
                members: vec![alice.public(), bob.public()],
                epoch: 1,
                owner: alice.public().user_id(),
            }
            .encode(),
        );

        let members_seen_by = |who: &DeviceIdentity| -> Vec<String> {
            let mut book = ChannelBook::new();
            book.process(who, channel, &collect(&log, &channel));
            let mut m: Vec<String> = book
                .state(&channel)
                .unwrap()
                .members()
                .iter()
                .map(|p| p.user_id())
                .collect();
            m.sort();
            m
        };
        // Every node — including whichever one the winner excludes — agrees on the membership.
        assert_eq!(members_seen_by(&bob), members_seen_by(&carol));
        assert_eq!(members_seen_by(&bob), members_seen_by(&alice));
    }
}
