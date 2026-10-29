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
    /// The last `limit` messages of a channel in deterministic causal order.
    /// Sender-key wire keys are single-use and gone after one decrypt, so history is
    /// served from the stores — our own sent plaintext from the sent sidecar, plus the
    /// plaintext of others' messages we decrypted on receipt (the received store). That
    /// IS the forward-secrecy property, not a limitation. Returns empty for an unknown
    /// channel.
    pub fn channel_history(
        &self,
        channel: ConversationId,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>, super::NodeError> {
        if !self
            .list_channels()?
            .iter()
            .any(|summary| summary.id == channel)
        {
            return Ok(Vec::new());
        }
        self.history(channel, limit)
    }

    /// The last `limit` messages of the DM with `peer`, both directions, in time
    /// order. Convenience wrapper that derives the conversation id.
    pub fn dm_history(
        &self,
        peer: &PublicIdentity,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>, super::NodeError> {
        let conv = dm_conversation_id(&self.identity.public(), peer);
        self.history(conv, limit)
    }

    /// Account-level conversation history with `peer_account_id`, merging our sent
    /// copies (recorded once under the account conversation) and the per-device copies
    /// we received and filed under it. `from_me` is derived from the recorded sender
    /// account, so self-synced copies of our own sends show as ours.
    pub fn account_history(
        &self,
        peer_account_id: &str,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>, super::NodeError> {
        self.project_history(
            account_conversation_id(&self.account.account_id(), peer_account_id),
            Some(peer_account_id),
            HistorySelection::Recent(limit),
        )
        .map(|context| context.map_or_else(Vec::new, |context| context.messages))
    }

    /// The last `limit` messages of `conversation`, both directions, sorted by
    /// wall-clock: our own sent plaintext from the local sidecar, plus the plaintext
    /// of peer-authored messages we decrypted on receipt (from the received store).
    /// The wire ratchet key is single-use and gone, so history is served from the
    /// stores — that IS the forward-secrecy property, not a limitation.
    pub fn history(
        &self,
        conversation: ConversationId,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>, super::NodeError> {
        self.project_history(conversation, None, HistorySelection::Recent(limit))
            .map(|context| context.map_or_else(Vec::new, |context| context.messages))
    }

    /// Build canonical identity/order from borrowed rows, then decode only the
    /// selected window. Metadata and causal traversal remain O(retained rows);
    /// copied plaintext is proportional to the selected rows, not retention.
    pub(in crate::node) fn project_history(
        &self,
        conversation: ConversationId,
        account: Option<&str>,
        selection: HistorySelection<'_>,
    ) -> Result<Option<super::search::SearchContext>, super::NodeError> {
        use super::delivery_store::DeliveryTransaction;
        use std::collections::{HashMap, HashSet};

        let selected_ids: Vec<EventId>;
        let selection = match selection {
            HistorySelection::Selected(ids) => {
                selected_ids = ids
                    .iter()
                    .map(|id| self.file_message_logical_id(conversation, *id))
                    .collect();
                HistorySelection::Selected(&selected_ids)
            }
            HistorySelection::Context(id) => {
                HistorySelection::Context(self.file_message_logical_id(conversation, id))
            }
            HistorySelection::Protected(id) => {
                HistorySelection::Protected(self.file_message_logical_id(conversation, id))
            }
            other => other,
        };
        let is_channel = account.is_none()
            && (self.channel_shared_reference(conversation).is_some()
                || self
                    .archived_channels
                    .iter()
                    .any(|ch| ch.id == conversation)
                || self
                    .channels
                    .lock()
                    .expect("channels mutex not poisoned")
                    .state(&conversation)
                    .is_some());
        let my_account = self.account.account_id();
        let me = self.identity.public().user_id();
        let own_devices: HashSet<_> = self
            .linked_devices()
            .into_iter()
            .map(|device| device.public.user_id())
            .collect();
        let recovered_me: HashSet<_> = self
            .recovery_origins
            .iter()
            .map(|author| author.user_id())
            .collect();
        // Match the delivery owner's lock ordering. No guard escapes this read.
        let delivery = self.delivery.lock().expect("delivery lock not poisoned");
        let log = self.log.lock().expect("log mutex not poisoned");
        let events = log.project_events(&conversation, |mut event| {
            event.ciphertext.clear();
            event.sig.clear();
            event
        })?;
        let author = crate::eventlog::Author::from_ed25519(self.identity.public().ed25519_pub);
        let mut sent_ids: HashMap<_, _> = if account.is_none() {
            events
                .iter()
                .filter(|event| {
                    event.kind == crate::eventlog::EventKind::Message && event.author == author
                })
                .map(|event| (event.seq, event.id))
                .collect()
        } else {
            HashMap::new()
        };
        let sentlog = self.sentlog.lock().expect("sentlog mutex not poisoned");
        let recovered = self
            .recovered_sent_ids
            .lock()
            .expect("recovered sent ids mutex not poisoned");
        let received = self.received.lock().expect("received mutex not poisoned");
        let files = self
            .received_files
            .lock()
            .expect("received_files mutex not poisoned");
        let pending = delivery.pending_transactions();
        for transaction in pending {
            if let DeliveryTransaction::Outgoing { message, sent, .. } = transaction.body()
                && sent.conversation == conversation
            {
                sent_ids.insert(sent.seq, message.logical_id);
            }
        }
        let pending_sent = pending
            .iter()
            .filter_map(|transaction| match transaction.body() {
                DeliveryTransaction::Outgoing { sent, .. } if sent.conversation == conversation => {
                    Some(sent)
                }
                _ => None,
            });
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        // Preserve the former sidecar-copy ordering before first-ID dedup.
        // Arrival order can disagree with wall clocks for sibling imports.
        let mut sent_rows: Vec<_> = sentlog.entries_ref(&conversation).iter().collect();
        sent_rows.sort_by_key(|row| (row.wall_clock, row.seq));
        for sent in sent_rows.into_iter().chain(pending_sent) {
            let envelope = DmEnvelope::decode_ref(&sent.plaintext);
            let inner = envelope
                .as_ref()
                .map_or(sent.plaintext.as_slice(), |env| env.body);
            let expires_at_ms = crate::expiry::deadline(inner);
            if expires_at_ms.is_some_and(|deadline| deadline <= self.expiry_now()) {
                continue;
            }
            let id = if account.is_some() {
                envelope
                    .as_ref()
                    .map_or(EventId::new([0; 32]), |env| EventId::new(env.msg_id))
            } else {
                recovered
                    .as_ref()
                    .and_then(|saved| saved.id_for(sent))
                    .or_else(|| sent_ids.get(&sent.seq).copied())
                    .unwrap_or(EventId::new([0; 32]))
            };
            if seen.insert(id) {
                rows.push(HistoryRow {
                    id,
                    wall_clock: sent.wall_clock,
                    from_me: true,
                    who: "you",
                    plaintext: if account.is_some() {
                        inner
                    } else {
                        &sent.plaintext
                    },
                    expires_at_ms,
                    file: false,
                });
            }
        }
        let mut received_rows: Vec<_> = received.entries_ref(&conversation).iter().collect();
        received_rows.sort_by_key(|row| row.wall_clock);
        for row in received_rows {
            let envelope = account.and_then(|_| DmEnvelope::decode_ref(&row.plaintext));
            let inner = envelope
                .as_ref()
                .map_or(row.plaintext.as_slice(), |env| env.body);
            if self.recipient_expired(inner, row.event_id, row.wall_clock) {
                continue;
            }
            let id = if account.is_some() {
                envelope
                    .as_ref()
                    .map_or(EventId::new([0; 32]), |env| EventId::new(env.msg_id))
            } else {
                row.event_id
            };
            let from_me = if account.is_some() {
                row.from == my_account
            } else {
                own_devices.contains(&row.from)
            };
            if seen.insert(id) {
                rows.push(HistoryRow {
                    id,
                    wall_clock: row.wall_clock,
                    from_me,
                    who: if from_me { "you" } else { &row.from },
                    plaintext: inner,
                    expires_at_ms: DmEnvelope::decode_ref(&row.plaintext).map_or_else(
                        || crate::expiry::deadline(&row.plaintext),
                        |envelope| crate::expiry::deadline(envelope.body),
                    ),
                    file: false,
                });
            }
        }
        let durable_files = files.entries_ref(&conversation);
        let pending_files = pending.iter().filter_map(|transaction| match transaction {
            DeliveryTransaction::OutgoingManifest { received, .. }
                if received.conversation == conversation
                    && !durable_files
                        .iter()
                        .any(|row| row.event_id == received.event_id) =>
            {
                Some(received.as_ref())
            }
            _ => None,
        });
        let identities = self
            .file_message_identities
            .lock()
            .expect("file identity lock");
        let suppression = self
            .account_suppression
            .lock()
            .expect("account suppression lock");
        let mut file_ids = HashSet::new();
        let mut file_rows: Vec<_> = durable_files.iter().collect();
        file_rows.sort_by_key(|row| row.wall_clock);
        for row in file_rows.into_iter().chain(pending_files) {
            if !file_ids.insert(row.event_id)
                || delivery.file_erased(row.conversation, row.event_id)
                || self.recipient_expired(&row.plaintext, row.event_id, row.wall_clock)
                || crate::file::decode_manifest(&row.plaintext).is_none()
            {
                continue;
            }
            let from_me = row.from == me
                || recovered_me.contains(&row.from)
                || own_devices.contains(&row.from);
            let logical = identities.row_logical(row);
            if suppression.contains(conversation, logical) {
                continue;
            }
            if seen.insert(logical) {
                rows.push(HistoryRow {
                    id: logical,
                    wall_clock: row.wall_clock,
                    from_me,
                    who: if from_me { "you" } else { &row.from },
                    plaintext: &row.plaintext,
                    expires_at_ms: crate::expiry::deadline(&row.plaintext),
                    file: true,
                });
            }
        }
        drop(suppression);
        drop(identities);
        // Channel file acceptance is durable before its history sidecar is
        // projected. Read the same retained manifest from that pending WAL entry.
        for transaction in pending {
            if let DeliveryTransaction::ChannelOutgoing { events, sent, .. } = transaction.body()
                && sent.conversation == conversation
                && let Some(event) = events.last()
                && event.kind == crate::eventlog::EventKind::FileManifest
                && !seen.contains(&event.id)
                && !delivery.file_erased(conversation, event.id)
                && !self.recipient_expired(&sent.plaintext, event.id, sent.wall_clock)
                && crate::file::decode_manifest(&sent.plaintext).is_some()
            {
                seen.insert(event.id);
                rows.push(HistoryRow {
                    id: event.id,
                    wall_clock: sent.wall_clock,
                    from_me: true,
                    who: "you",
                    plaintext: &sent.plaintext,
                    expires_at_ms: crate::expiry::deadline(&sent.plaintext),
                    file: true,
                });
            }
        }
        if is_channel {
            let archive: Vec<_> = rows.iter().map(|row| (row.id, row.wall_clock)).collect();
            let ranks: HashMap<_, _> =
                super::timeline::ordered_ids(&events.iter().collect::<Vec<_>>(), &archive, false)
                    .into_iter()
                    .enumerate()
                    .map(|(rank, id)| (id, rank))
                    .collect();
            rows.sort_by_key(|row| ranks.get(&row.id).copied().unwrap_or(usize::MAX));
        } else {
            rows.sort_by_key(|row| (row.wall_clock, row.id));
        }
        if let HistorySelection::Selected(ids) = selection {
            rows.retain(|row| ids.contains(&row.id));
        }
        let (start, end, newer_entries) = match selection {
            HistorySelection::Selected(_) => (0, rows.len(), 0),
            HistorySelection::Recent(limit) => (rows.len().saturating_sub(limit), rows.len(), 0),
            HistorySelection::Protected(id) => {
                let Some(target) = rows.iter().position(|row| row.id == id) else {
                    return Ok(None);
                };
                (target, target + 1, rows.len() - target - 1)
            }
            HistorySelection::Context(id) => {
                let Some(target) = rows.iter().position(|row| row.id == id) else {
                    return Ok(None);
                };
                (
                    target.saturating_sub(25),
                    target.saturating_add(26).min(rows.len()),
                    rows.len() - target - 1,
                )
            }
        };
        let mut messages: Vec<_> = rows[start..end]
            .iter()
            .map(|row| {
                let mut entry = row.decode();
                entry.reply_to = entry
                    .reply_to
                    .map(|target| self.file_message_logical_id(conversation, target));
                if row.file {
                    entry.file_message_aliases = self
                        .file_message_identities
                        .lock()
                        .expect("file identity lock")
                        .aliases(conversation, row.id);
                }
                if let HistorySelection::Protected(id) = selection
                    && row.id == id
                    && entry.protection.is_some()
                    && entry.file.is_none()
                {
                    entry.text = MessageBody::decode_revealed(row.plaintext).text;
                }
                entry
            })
            .collect();
        // Recall resolution can acquire the same stores. Release the borrowed
        // snapshot first, then apply the latest authorized recall projection.
        drop(rows);
        drop(events);
        drop(files);
        drop(received);
        drop(recovered);
        drop(sentlog);
        drop(log);
        drop(delivery);
        let recalled = if let Some(peer) = account {
            self.recalled_targets_account(peer)?
        } else if is_channel {
            self.recalled_targets_channel(conversation)?
        } else {
            HashSet::new()
        };
        if let HistorySelection::Context(id) | HistorySelection::Protected(id) = selection
            && recalled.contains(&id)
        {
            return Ok(None);
        }
        tombstone_recalled(&mut messages, &recalled);
        if matches!(
            selection,
            HistorySelection::Context(_) | HistorySelection::Protected(_)
        ) {
            for entry in &mut messages {
                entry.recalled_text = None;
            }
        }
        Ok(Some(super::search::SearchContext {
            messages,
            newer_entries,
        }))
    }
}

#[derive(Clone, Copy)]
pub(in crate::node) enum HistorySelection<'a> {
    Selected(&'a [EventId]),
    Recent(usize),
    Context(EventId),
    Protected(EventId),
}

struct HistoryRow<'a> {
    id: EventId,
    wall_clock: u64,
    from_me: bool,
    who: &'a str,
    plaintext: &'a [u8],
    expires_at_ms: Option<u64>,
    file: bool,
}

impl HistoryRow<'_> {
    fn decode(&self) -> HistoryEntry {
        #[cfg(test)]
        PROJECTION_DECODES.with(|count| count.set(count.get() + 1));
        let body = if self.file {
            MessageBody::new(Vec::new(), None)
        } else {
            MessageBody::decode(self.plaintext)
        };
        let file = if self.file {
            let manifest =
                crate::file::decode_manifest(self.plaintext).expect("validated file history");
            Some(FileHistoryInfo {
                file_conv: manifest.file_conv(),
                name: if manifest.protection().is_some() {
                    "Protected image".into()
                } else {
                    manifest.name().to_string()
                },
                size: manifest.size(),
                mime: manifest.mime().to_string(),
                media: crate::node::media_store::manifest_is_media(&manifest),
            })
        } else {
            None
        };
        HistoryEntry {
            id: self.id,
            file_message_aliases: Vec::new(),
            wall_clock: self.wall_clock,
            from_me: self.from_me,
            who: self.who.to_string(),
            text: body.text,
            protection: if self.file {
                crate::file::decode_manifest(self.plaintext).and_then(|m| m.protection())
            } else {
                body.protection
            },
            reply_to: body.reply_to,
            expires_at_ms: self.expires_at_ms,
            file,
            recalled: false,
            recalled_text: None,
            sticker: body.sticker,
        }
    }
}

#[cfg(test)]
thread_local! { static PROJECTION_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
pub(super) fn reset_projection_decodes() {
    PROJECTION_DECODES.with(|count| count.set(0));
}
#[cfg(test)]
pub(super) fn projection_decodes() -> usize {
    PROJECTION_DECODES.with(std::cell::Cell::get)
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
