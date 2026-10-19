//! Read-only conversation queries on [`Node`]: per-conversation history (channel, DM,
//! account), and the unified `history`. Full-text search lives in `search.rs`.
//! All are pure reads that serve plaintext from the sent/received stores — the wire
//! keys are single-use and gone, which IS the forward-secrecy property, not a gap.

use super::conversation::{account_conversation_id, dm_conversation_id};
use super::node::FileHistoryInfo;
use super::{DmEnvelope, HistoryEntry, MessageBody, Node};
use crate::eventlog::event::{ConversationId, EventId};
use crate::identity::device::PublicIdentity;

impl Node {
    /// File/media messages (`FileManifest` events) of `conversation`, as history entries
    /// ordered with text by their callers. Sources the durable `received_files` store,
    /// which now holds both files we SENT (recorded at stage time, `from_me`) and files we
    /// RECEIVED (recorded on open), keyed by their host (DM/channel) conversation. Each
    /// entry carries the file metadata the UI needs to render inline media or a file card;
    /// `text` is empty. `from_me` is true when the recorded author is this device.
    pub(in crate::node) fn conversation_files(
        &self,
        conversation: ConversationId,
    ) -> Vec<HistoryEntry> {
        let me = self.identity.public().user_id();
        let recovered_me: std::collections::HashSet<_> = self
            .recovery_origins
            .iter()
            .map(|author| author.user_id())
            .collect();
        let linked_me: std::collections::HashSet<_> = self
            .linked_devices()
            .into_iter()
            .map(|device| device.public.user_id())
            .collect();
        let delivery = self.delivery.lock().expect("delivery lock not poisoned");
        let mut entries = self
            .received_files
            .lock()
            .expect("received_files mutex not poisoned")
            .entries(&conversation);
        for transaction in delivery.pending_transactions() {
            if let super::delivery_store::DeliveryTransaction::OutgoingManifest { received, .. } =
                transaction
                && received.conversation == conversation
                && !entries
                    .iter()
                    .any(|entry| entry.event_id == received.event_id)
            {
                entries.push((**received).clone());
            }
        }
        entries
            .into_iter()
            .filter(|entry| !delivery.file_erased(entry.conversation, entry.event_id))
            .filter(|entry| {
                entry.from == me
                    || linked_me.contains(&entry.from)
                    || recovered_me.contains(&entry.from)
                    || !self.recipient_expired(&entry.plaintext, entry.event_id, entry.wall_clock)
            })
            .filter_map(|entry| {
                let manifest = crate::file::decode_manifest(&entry.plaintext)?;
                let from_me = entry.from == me
                    || recovered_me.contains(&entry.from)
                    || linked_me.contains(&entry.from);
                Some(HistoryEntry {
                    id: entry.event_id,
                    from_me,
                    who: if from_me {
                        "you".to_string()
                    } else {
                        entry.from
                    },
                    text: Vec::new(),
                    wall_clock: entry.wall_clock,
                    reply_to: None,
                    file: Some(FileHistoryInfo {
                        file_conv: manifest.file_conv(),
                        name: manifest.name().to_string(),
                        size: manifest.size(),
                        mime: manifest.mime().to_string(),
                        media: crate::node::media_store::manifest_is_media(&manifest),
                    }),
                    recalled: false,
                    recalled_text: None,
                    sticker: None,
                })
            })
            .collect()
    }

    /// The last `limit` messages of a channel in deterministic causal order.
    /// Sender-key wire keys are single-use and gone after one decrypt, so history is
    /// served from the stores — our own sent plaintext from the sent sidecar, plus the
    /// plaintext of others' messages we decrypted on receipt (the received store). That
    /// IS the forward-secrecy property, not a limitation. Returns empty for an unknown
    /// channel.
    pub fn channel_history(&self, channel: ConversationId, limit: usize) -> Vec<HistoryEntry> {
        if !self
            .list_channels()
            .iter()
            .any(|summary| summary.id == channel)
        {
            return Vec::new();
        }
        self.history(channel, limit)
    }

    /// The last `limit` messages of the DM with `peer`, both directions, in time
    /// order. Convenience wrapper that derives the conversation id.
    pub fn dm_history(&self, peer: &PublicIdentity, limit: usize) -> Vec<HistoryEntry> {
        let conv = dm_conversation_id(&self.identity.public(), peer);
        self.history(conv, limit)
    }

    /// Account-level conversation history with `peer_account_id`, merging our sent
    /// copies (recorded once under the account conversation) and the per-device copies
    /// we received and filed under it. `from_me` is derived from the recorded sender
    /// account, so self-synced copies of our own sends show as ours.
    pub fn account_history(&self, peer_account_id: &str, limit: usize) -> Vec<HistoryEntry> {
        let my_account = self.account.account_id();
        let conv = account_conversation_id(&my_account, peer_account_id);
        let mut entries: Vec<HistoryEntry> = Vec::new();

        // Account messages are stored as full `DmEnvelope`s; the logical `msg_id` is
        // the stable, cross-device id reactions/replies target.
        for sent in self.sent_entries_for_history(conv) {
            let (id, body) = decode_account_entry(&sent.plaintext);
            entries.push(HistoryEntry {
                id,
                from_me: true,
                who: "you".to_string(),
                text: body.text,
                wall_clock: sent.wall_clock,
                reply_to: body.reply_to,
                file: None,
                recalled: false,
                recalled_text: None,
                sticker: body.sticker,
            });
        }

        for rcv in self
            .received
            .lock()
            .expect("received mutex not poisoned")
            .entries(&conv)
        {
            let envelope = super::dm_envelope::DmEnvelope::decode(&rcv.plaintext);
            let inner = envelope
                .as_ref()
                .map_or(rcv.plaintext.as_slice(), |e| e.body.as_slice());
            if rcv.from != my_account && self.recipient_expired(inner, rcv.event_id, rcv.wall_clock)
            {
                continue;
            }
            let (id, body) = decode_account_entry(&rcv.plaintext);
            let from_me = rcv.from == my_account;
            let hidden = !from_me && self.hidden_by_language(rcv.event_id);
            let body = if hidden {
                MessageBody::new(super::language_io::HIDDEN_TEXT.to_vec(), None)
            } else {
                body
            };
            entries.push(HistoryEntry {
                id,
                from_me,
                who: if from_me { "you".to_string() } else { rcv.from },
                text: body.text,
                wall_clock: rcv.wall_clock,
                reply_to: body.reply_to,
                file: None,
                recalled: false,
                recalled_text: None,
                sticker: body.sticker,
            });
        }

        // Merge file/media messages (FileManifest events) so they're ordered inline with
        // text by wall-clock.
        entries.extend(self.conversation_files(conv));

        // A logical account DM can arrive directly and later be imported from a
        // sibling with a different transport-event id. Keep its first local copy.
        let mut seen = std::collections::HashSet::new();
        entries.retain(|entry| seen.insert(entry.id));

        // Tombstone recalled messages: keep the slot (sender + position) but drop the content.
        tombstone_recalled(
            &mut entries,
            &self.recalled_targets_account(peer_account_id),
        );

        // Tie-break equal wall-clocks by the (global, content-addressed) event id so
        // both participants render the same order on a same-millisecond collision.
        entries.sort_by(|a, b| {
            a.wall_clock
                .cmp(&b.wall_clock)
                .then_with(|| a.id.cmp(&b.id))
        });
        if entries.len() > limit {
            entries.drain(0..entries.len() - limit);
        }
        entries
    }

    /// The last `limit` messages of `conversation`, both directions, sorted by
    /// wall-clock: our own sent plaintext from the local sidecar, plus the plaintext
    /// of peer-authored messages we decrypted on receipt (from the received store).
    /// The wire ratchet key is single-use and gone, so history is served from the
    /// stores — that IS the forward-secrecy property, not a limitation.
    pub fn history(&self, conversation: ConversationId, limit: usize) -> Vec<HistoryEntry> {
        let mut entries: Vec<HistoryEntry> = Vec::new();
        let own_devices: std::collections::HashSet<_> = self
            .linked_devices()
            .into_iter()
            .map(|device| device.public.user_id())
            .collect();
        for (id, sent) in self.raw_sent_history(conversation) {
            let body = MessageBody::decode(&sent.plaintext);
            entries.push(HistoryEntry {
                id,
                from_me: true,
                who: "you".to_string(),
                text: body.text,
                wall_clock: sent.wall_clock,
                reply_to: body.reply_to,
                file: None,
                recalled: false,
                recalled_text: None,
                sticker: body.sticker,
            });
        }
        for rcv in self
            .received
            .lock()
            .expect("received mutex not poisoned")
            .entries(&conversation)
        {
            if !own_devices.contains(&rcv.from)
                && self.recipient_expired(&rcv.plaintext, rcv.event_id, rcv.wall_clock)
            {
                continue;
            }
            let body = MessageBody::decode(&rcv.plaintext);
            let from_me = own_devices.contains(&rcv.from);
            let body = if !from_me && self.hidden_by_language(rcv.event_id) {
                MessageBody::new(super::language_io::HIDDEN_TEXT.to_vec(), None)
            } else {
                body
            };
            entries.push(HistoryEntry {
                id: rcv.event_id,
                from_me,
                who: if from_me { "you".to_string() } else { rcv.from },
                text: body.text,
                wall_clock: rcv.wall_clock,
                reply_to: body.reply_to,
                file: None,
                recalled: false,
                recalled_text: None,
                sticker: body.sticker,
            });
        }
        // Merge file/media messages (FileManifest events) so they're ordered inline with
        // text by wall-clock.
        entries.extend(self.conversation_files(conversation));
        let mut seen = std::collections::HashSet::new();
        entries.retain(|entry| seen.insert(entry.id));
        // Tombstone recalled messages. Recall is only wired for channels (1:1 chats go
        // through `account_history`); a device-pair DM has no Delete events, so this is an
        // empty set there.
        let is_channel = self
            .archived_channels
            .iter()
            .any(|summary| summary.id == conversation)
            || {
                let book = self.channels.lock().expect("channels mutex not poisoned");
                book.state(&conversation).is_some()
            };
        if is_channel {
            tombstone_recalled(&mut entries, &self.recalled_targets_channel(conversation));
        }
        // Tie-break equal wall-clocks by the (global, content-addressed) event id so
        // both participants render the same order on a same-millisecond collision.
        if is_channel {
            let log = self.log.lock().expect("log mutex not poisoned");
            let events = log.events(&conversation);
            let archive: Vec<_> = entries
                .iter()
                .map(|entry| (entry.id, entry.wall_clock))
                .collect();
            let order = super::timeline::ordered_ids(&events, &archive, false);
            let ranks: std::collections::HashMap<_, _> = order
                .into_iter()
                .enumerate()
                .map(|(rank, id)| (id, rank))
                .collect();
            entries.sort_by_key(|entry| ranks.get(&entry.id).copied().unwrap_or(usize::MAX));
        } else {
            entries.sort_by(|a, b| {
                a.wall_clock
                    .cmp(&b.wall_clock)
                    .then_with(|| a.id.cmp(&b.id))
            });
        }
        if entries.len() > limit {
            entries.drain(0..entries.len() - limit);
        }
        entries
    }
}

/// Blank the content of any entry whose id was recalled, keeping its slot (sender +
/// position) so the UI can render an "X recalled a message" placeholder. The plaintext /
/// file metadata of a recalled message is never returned.
fn tombstone_recalled(entries: &mut [HistoryEntry], recalled: &std::collections::HashSet<EventId>) {
    for e in entries.iter_mut() {
        if recalled.contains(&e.id) {
            e.recalled = true;
            // For OUR OWN recalled text messages, keep the original text re-editable
            // (WeChat's "re-edit"). Never expose a peer's recalled content, a file, or a
            // sticker (re-editing a sticker into the text box makes no sense).
            if e.from_me && e.file.is_none() && e.sticker.is_none() && !e.text.is_empty() {
                e.recalled_text = Some(std::mem::take(&mut e.text));
            }
            e.text = Vec::new();
            e.file = None;
            e.reply_to = None;
            e.sticker = None;
        }
    }
}

/// Decode an account-history store entry (a full `DmEnvelope`) into its logical id
/// and inner body. A malformed or legacy entry falls back to a bare `MessageBody`
/// with a zero id, so it still renders rather than breaking history.
pub(in crate::node) fn decode_account_entry(plaintext: &[u8]) -> (EventId, MessageBody) {
    match DmEnvelope::decode(plaintext) {
        Some(env) => (EventId::new(env.msg_id), MessageBody::decode(&env.body)),
        None => (EventId::new([0u8; 32]), MessageBody::decode(plaintext)),
    }
}
