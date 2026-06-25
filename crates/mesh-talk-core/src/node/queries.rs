//! Read-only conversation queries on [`Node`]: per-conversation history (channel, DM,
//! account), the unified `history`, and full-text `search`. Split out of `node.rs`.
//! All are pure reads that serve plaintext from the sent/received stores — the wire
//! keys are single-use and gone, which IS the forward-secrecy property, not a gap.

use super::conversation::{account_conversation_id, dm_conversation_id};
use super::node::FileHistoryInfo;
use super::{DmEnvelope, HistoryEntry, MessageBody, Node, SearchHit};
use crate::eventlog::event::{Author, ConversationId, EventId, EventKind};
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
        self.received_files
            .lock()
            .expect("received_files mutex not poisoned")
            .entries(&conversation)
            .into_iter()
            .filter_map(|entry| {
                let manifest = crate::file::decode_manifest(&entry.plaintext)?;
                let from_me = entry.from == me;
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
                })
            })
            .collect()
    }

    /// The last `limit` messages of a channel (all directions), sorted by wall-clock.
    /// Sender-key wire keys are single-use and gone after one decrypt, so history is
    /// served from the stores — our own sent plaintext from the sent sidecar, plus the
    /// plaintext of others' messages we decrypted on receipt (the received store). That
    /// IS the forward-secrecy property, not a limitation. Returns empty for an unknown
    /// channel.
    pub fn channel_history(&self, channel: ConversationId, limit: usize) -> Vec<HistoryEntry> {
        {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            if book.state(&channel).is_none() {
                return Vec::new();
            }
        }
        // A channel conversation's history is built exactly like a DM's (sent sidecar +
        // received store, seq->id mapped, wall-clock + id sorted) — so delegate to `history`
        // rather than duplicate it.
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
        for sent in self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .entries(&conv)
        {
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
            });
        }

        for rcv in self
            .received
            .lock()
            .expect("received mutex not poisoned")
            .entries(&conv)
        {
            let (id, body) = decode_account_entry(&rcv.plaintext);
            let from_me = rcv.from == my_account;
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
            });
        }

        // Merge file/media messages (FileManifest events) so they're ordered inline with
        // text by wall-clock.
        entries.extend(self.conversation_files(conv));

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
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        // Map our own Message events' seq -> id, to give sent sidecar entries an id.
        let mut my_msg_ids: std::collections::HashMap<u64, EventId> =
            std::collections::HashMap::new();
        {
            let log = self.log.lock().expect("log mutex not poisoned");
            for event in log.events(&conversation) {
                if event.kind == EventKind::Message && event.author == self_author {
                    my_msg_ids.insert(event.seq, event.id);
                }
            }
        }

        let mut entries: Vec<HistoryEntry> = Vec::new();
        for sent in self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .entries(&conversation)
        {
            let body = MessageBody::decode(&sent.plaintext);
            entries.push(HistoryEntry {
                id: my_msg_ids
                    .get(&sent.seq)
                    .copied()
                    .unwrap_or(EventId::new([0u8; 32])),
                from_me: true,
                who: "you".to_string(),
                text: body.text,
                wall_clock: sent.wall_clock,
                reply_to: body.reply_to,
                file: None,
                recalled: false,
                recalled_text: None,
            });
        }
        for rcv in self
            .received
            .lock()
            .expect("received mutex not poisoned")
            .entries(&conversation)
        {
            let body = MessageBody::decode(&rcv.plaintext);
            entries.push(HistoryEntry {
                id: rcv.event_id,
                from_me: false,
                who: rcv.from,
                text: body.text,
                wall_clock: rcv.wall_clock,
                reply_to: body.reply_to,
                file: None,
                recalled: false,
                recalled_text: None,
            });
        }
        // Merge file/media messages (FileManifest events) so they're ordered inline with
        // text by wall-clock.
        entries.extend(self.conversation_files(conversation));
        // Tombstone recalled messages. Recall is only wired for channels (1:1 chats go
        // through `account_history`); a device-pair DM has no Delete events, so this is an
        // empty set there.
        let is_channel = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            book.state(&conversation).is_some()
        };
        if is_channel {
            tombstone_recalled(&mut entries, &self.recalled_targets_channel(conversation));
        }
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

    /// Search decrypted history across all known DMs + channels for `query`
    /// (case-insensitive substring). Empty/whitespace query → no hits.
    ///
    /// Scans the sent + received stores BY REFERENCE — it never clones a full
    /// conversation Vec, only the plaintext of the lines that actually match — and skips
    /// the seq->id log scan `history` does (search results carry no event id), so a
    /// keystroke no longer copies every plaintext in every conversation.
    pub fn search(&self, query: &str) -> Vec<SearchHit> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        const PER_CONV: usize = 2000;
        let mut hits: Vec<SearchHit> = Vec::new();

        let peers = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .peers();
        for peer in peers.iter().filter(|p| !p.post_office) {
            let conv = dm_conversation_id(&self.identity.public(), &peer.public);
            for m in self.scan_conversation(conv, &q, PER_CONV, false) {
                hits.push(SearchHit {
                    is_channel: false,
                    target: peer.public.user_id(),
                    label: peer.name.clone(),
                    from_me: m.from_me,
                    who: m.who,
                    text: m.text,
                    wall_clock: m.wall_clock,
                });
            }
        }
        // Account-addressed DMs (the multi-device path the app uses) live under account
        // conversations, not device-pair ones — scan them too, deduped by account id.
        let my_account = self.account.account_id();
        let mut seen_accounts = std::collections::HashSet::new();
        for peer in peers.iter().filter(|p| !p.post_office) {
            let Some(acct) = peer.account_id.clone() else {
                continue;
            };
            if !seen_accounts.insert(acct.clone()) {
                continue;
            }
            let conv = account_conversation_id(&my_account, &acct);
            for m in self.scan_conversation(conv, &q, PER_CONV, true) {
                hits.push(SearchHit {
                    is_channel: false,
                    target: acct.clone(),
                    label: peer.name.clone(),
                    from_me: m.from_me,
                    who: m.who,
                    text: m.text,
                    wall_clock: m.wall_clock,
                });
            }
        }
        for ch in self.list_channels() {
            for m in self.scan_conversation(ch.id, &q, PER_CONV, false) {
                hits.push(SearchHit {
                    is_channel: true,
                    target: hex::encode(ch.id.as_bytes()),
                    label: ch.name.clone(),
                    from_me: m.from_me,
                    who: m.who,
                    text: m.text,
                    wall_clock: m.wall_clock,
                });
            }
        }
        hits.sort_by_key(|b| std::cmp::Reverse(b.wall_clock)); // most recent first
        hits
    }

    /// Scan one conversation's sent + received plaintext for a lowercased substring `q`,
    /// matching the same most-recent-`limit` window and the same `from_me`/`who`/`text`
    /// derivation as [`Node::history`] / [`Node::account_history`], but borrowing the
    /// stores (no full-Vec clone) and cloning only the matching lines. `account` selects
    /// the account-conversation envelope decode + sender derivation.
    fn scan_conversation(
        &self,
        conv: ConversationId,
        q: &str,
        limit: usize,
        account: bool,
    ) -> Vec<ScanMatch> {
        let my_account = self.account.account_id();
        // Materialize (wall_clock, from_me, who, text) for the window, borrowing the
        // stores; only the decoded body bytes of kept lines are owned. Mirror `history`'s
        // ordering (wall_clock, then a sent-before-received tiebreak is irrelevant to a
        // substring scan) and most-recent-`limit` truncation.
        let mut lines: Vec<(u64, bool, String, Vec<u8>)> = Vec::new();
        {
            let sentlog = self.sentlog.lock().expect("sentlog mutex not poisoned");
            for e in sentlog.entries_ref(&conv) {
                let (from_me, who, body) = if account {
                    let (_, body) = decode_account_entry(&e.plaintext);
                    (true, "you".to_string(), body.text)
                } else {
                    (
                        true,
                        "you".to_string(),
                        MessageBody::decode(&e.plaintext).text,
                    )
                };
                lines.push((e.wall_clock, from_me, who, body));
            }
        }
        {
            let received = self.received.lock().expect("received mutex not poisoned");
            for e in received.entries_ref(&conv) {
                let (from_me, who, body) = if account {
                    let (_, body) = decode_account_entry(&e.plaintext);
                    let from_me = e.from == my_account;
                    let who = if from_me {
                        "you".to_string()
                    } else {
                        e.from.clone()
                    };
                    (from_me, who, body.text)
                } else {
                    (
                        false,
                        e.from.clone(),
                        MessageBody::decode(&e.plaintext).text,
                    )
                };
                lines.push((e.wall_clock, from_me, who, body));
            }
        }
        lines.sort_by_key(|l| l.0);
        if lines.len() > limit {
            lines.drain(0..lines.len() - limit);
        }
        lines
            .into_iter()
            .filter(|(_, _, _, text)| String::from_utf8_lossy(text).to_lowercase().contains(q))
            .map(|(wall_clock, from_me, who, text)| ScanMatch {
                from_me,
                who,
                text,
                wall_clock,
            })
            .collect()
    }
}

/// One matching line from [`Node::scan_conversation`] (the fields `search` needs).
struct ScanMatch {
    from_me: bool,
    who: String,
    text: Vec<u8>,
    wall_clock: u64,
}

/// Decode an account-history store entry (a full `DmEnvelope`) into its logical id
/// and inner body. A malformed or legacy entry falls back to a bare `MessageBody`
/// with a zero id, so it still renders rather than breaking history.
/// Blank the content of any entry whose id was recalled, keeping its slot (sender +
/// position) so the UI can render an "X recalled a message" placeholder. The plaintext /
/// file metadata of a recalled message is never returned.
fn tombstone_recalled(entries: &mut [HistoryEntry], recalled: &std::collections::HashSet<EventId>) {
    for e in entries.iter_mut() {
        if recalled.contains(&e.id) {
            e.recalled = true;
            // For OUR OWN recalled text messages, keep the original text re-editable
            // (WeChat's "re-edit"). Never expose a peer's recalled content, or a file.
            if e.from_me && e.file.is_none() && !e.text.is_empty() {
                e.recalled_text = Some(std::mem::take(&mut e.text));
            }
            e.text = Vec::new();
            e.file = None;
            e.reply_to = None;
        }
    }
}

pub(in crate::node) fn decode_account_entry(plaintext: &[u8]) -> (EventId, MessageBody) {
    match DmEnvelope::decode(plaintext) {
        Some(env) => (EventId::new(env.msg_id), MessageBody::decode(&env.body)),
        None => (EventId::new([0u8; 32]), MessageBody::decode(plaintext)),
    }
}
