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

    /// Canonical metadata ranks persist across ordinary reads. SQL selects the
    /// requested ranks and admits their byte cost before any transcript body load.
    pub(in crate::node) fn project_history(
        &self,
        conversation: ConversationId,
        account: Option<&str>,
        selection: HistorySelection<'_>,
    ) -> Result<Option<super::search::SearchContext>, super::NodeError> {
        use super::delivery_store::DeliveryTransaction;
        use super::transcript_repository::{FILES, RECEIVED, SENT, TranscriptPageLimit};
        use crate::eventlog::repository::order::{
            CausalOrder, OrderReadiness, OrderWorkBudget, combined::HistorySelector,
        };
        use std::collections::{HashMap, HashSet};
        let selected_ids: Vec<EventId>;
        let selection = match selection {
            HistorySelection::Selected(ids) => {
                if ids
                    .len()
                    .checked_mul(std::mem::size_of::<EventId>())
                    .is_none_or(|bytes| bytes > 8 * 1024 * 1024)
                {
                    return Err(crate::eventlog::LogError::InvalidPageBudget.into());
                }
                selected_ids = ids
                    .iter()
                    .map(|id| self.file_message_logical_id(conversation, *id))
                    .collect::<Result<_, _>>()?;
                HistorySelection::Selected(&selected_ids)
            }
            HistorySelection::Context(id) => {
                HistorySelection::Context(self.file_message_logical_id(conversation, id)?)
            }
            HistorySelection::Protected(id) => {
                HistorySelection::Protected(self.file_message_logical_id(conversation, id)?)
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
                    .expect("channels mutex")
                    .state(&conversation)
                    .is_some());
        let now = self.expiry_now();
        let me = self.user_id();
        let my_account = self.account_id();
        let own_devices: HashSet<_> = self
            .linked_devices()
            .into_iter()
            .map(|d| d.public.user_id())
            .collect();
        let recovered_me: HashSet<_> = self.recovery_origins.iter().map(|a| a.user_id()).collect();
        let file_book = self.files.lock().expect("file book mutex").clone();
        let delivery = self.delivery.lock().expect("delivery mutex");
        let log = self.log.lock().expect("log mutex");
        let repository = log.repository()?;
        let sent = self.sentlog.lock().expect("sent mutex");
        let recovered = self.recovered_sent_ids.lock().expect("recovered ids mutex");
        let received = self.received.lock().expect("received mutex");
        let files = self.received_files.lock().expect("file transcript mutex");
        let identities = self
            .file_message_identities
            .lock()
            .expect("file identity mutex");
        let suppression = self.account_suppression.lock().expect("suppression mutex");
        let pending = delivery.pending_transactions();
        let pending_ids: HashMap<_, _> = pending
            .iter()
            .filter_map(|tx| match tx.body() {
                DeliveryTransaction::Outgoing { message, sent, .. }
                    if sent.conversation == conversation =>
                {
                    Some((sent.seq, message.logical_id))
                }
                _ => None,
            })
            .collect();
        let author = crate::eventlog::Author::from_ed25519(self.identity.public().ed25519_pub);
        let mut token = Vec::new();
        token.extend(my_account.as_bytes());
        token.extend(self.identity.public().ed25519_pub);
        token.push(u8::from(account.is_some()));
        token.push(u8::from(is_channel));
        for revision in [
            sent.conversation_revision(&conversation)?,
            received.conversation_revision(&conversation)?,
            files.conversation_revision(&conversation)?,
            file_book.revision()?,
            recovered
                .as_ref()
                .map(|r| r.revision())
                .transpose()?
                .unwrap_or(0),
            identities.revision()?,
            suppression.len()? as u64,
            repository
                .metadata(&conversation)?
                .map_or(0, |m| m.revision as u64),
        ] {
            token.extend(revision.to_be_bytes());
        }
        for tx in pending {
            token.extend(tx.id().as_bytes());
        }
        repository.initialize_history_order()?;
        let signed = if is_channel {
            while repository.ensure_order(&conversation, CausalOrder::WallClock)?
                != OrderReadiness::Ready
            {
                repository.order_step(
                    &conversation,
                    CausalOrder::WallClock,
                    OrderWorkBudget::default(),
                )?;
            }
            repository.order_generation(&conversation, CausalOrder::WallClock)?
        } else {
            None
        };
        let mut generation = repository.history_generation(&conversation, &token, now, signed)?;
        if !generation.ready {
            for kind in [SENT, RECEIVED, FILES] {
                let mut after = None;
                loop {
                    let page = match kind {
                        SENT => sent.header_scan_page(
                            Some(&conversation),
                            after,
                            TranscriptPageLimit::default(),
                        )?,
                        RECEIVED => received.header_scan_page(
                            Some(&conversation),
                            after,
                            TranscriptPageLimit::default(),
                        )?,
                        _ => files.header_scan_page(
                            Some(&conversation),
                            after,
                            TranscriptPageLimit::default(),
                        )?,
                    };
                    if page.rows.is_empty() {
                        break;
                    }
                    for header in page.rows {
                        after = Some(header.ordinal);
                        if kind == FILES && !file_book.eligible_ordinal(header.ordinal)? {
                            continue;
                        }
                        if header.deadline.is_some_and(|d| d <= now) {
                            continue;
                        }
                        let original = if kind == SENT {
                            None
                        } else {
                            Some(EventId::new(header.key.as_slice().try_into().map_err(
                                |_| {
                                    crate::eventlog::LogError::CorruptFile(
                                        "invalid history event key".into(),
                                    )
                                },
                            )?))
                        };
                        let id = if kind == SENT {
                            if account.is_some() {
                                header.logical_id.unwrap_or(EventId::new([0; 32]))
                            } else {
                                recovered
                                    .as_ref()
                                    .map(|r| r.id_for_header(&header))
                                    .transpose()?
                                    .flatten()
                                    .or(pending_ids.get(&header.sequence).copied())
                                    .or(repository
                                        .authored_header(&conversation, author, header.sequence)?
                                        .map(|h| h.id))
                                    .unwrap_or(EventId::new([0; 32]))
                            }
                        } else if kind == RECEIVED {
                            if account.is_some() {
                                header.logical_id.unwrap_or(EventId::new([0; 32]))
                            } else {
                                original.unwrap()
                            }
                        } else {
                            if header.file_conversation.is_none()
                                || delivery.file_erased(conversation, original.unwrap())
                            {
                                continue;
                            }
                            identities
                                .logical_for_ordinal(header.ordinal)?
                                .ok_or_else(|| {
                                    crate::eventlog::LogError::CorruptFile(
                                        "file selector disappeared".into(),
                                    )
                                })?
                        };
                        if suppression.contains(conversation, id)? {
                            continue;
                        }
                        let bytes = header
                            .body_bytes
                            .checked_add(header.sender_bytes)
                            .and_then(|n| n.checked_mul(3))
                            .and_then(|n| {
                                n.checked_add(if kind == FILES { 32 * 1024 } else { 512 })
                            })
                            .ok_or(crate::eventlog::LogError::RepositoryCounterExhausted)?;
                        repository.history_add(
                            &conversation,
                            generation.job,
                            HistorySelector {
                                id,
                                clock: header.wall_clock,
                                source_kind: kind,
                                ordinal: header.ordinal,
                                priority: kind * 2,
                                sequence: header.sequence,
                                charged_bytes: bytes,
                            },
                            header.deadline,
                        )?;
                    }
                }
            }
            // The delivery journal is already bounded to 32 transactions/8 MiB.
            // Pending accepted rows remain visible until their ordinary projection commits.
            for (index, tx) in pending.iter().enumerate() {
                let row = match tx.body() {
                    DeliveryTransaction::Outgoing { message, sent, .. }
                        if sent.conversation == conversation =>
                    {
                        Some((
                            message.logical_id,
                            sent.wall_clock,
                            4,
                            3,
                            sent.seq,
                            sent.plaintext.as_slice(),
                        ))
                    }
                    DeliveryTransaction::OutgoingManifest { received, .. }
                        if received.conversation == conversation =>
                    {
                        Some((
                            identities.row_logical(received)?,
                            received.wall_clock,
                            5,
                            7,
                            0,
                            received.plaintext.as_slice(),
                        ))
                    }
                    DeliveryTransaction::ChannelOutgoing { events, sent, .. }
                        if sent.conversation == conversation
                            && events.last().is_some_and(|e| {
                                e.kind == crate::eventlog::EventKind::FileManifest
                            }) =>
                    {
                        Some((
                            events.last().unwrap().id,
                            sent.wall_clock,
                            6,
                            8,
                            sent.seq,
                            sent.plaintext.as_slice(),
                        ))
                    }
                    _ => None,
                };
                let Some((id, clock, kind, priority, sequence, plain)) = row else {
                    continue;
                };
                let deadline = super::expiry_policy::plaintext_deadline(plain);
                if deadline.is_some_and(|d| d <= now)
                    || suppression.contains(conversation, id)?
                    || (kind != 4 && delivery.file_erased(conversation, id))
                {
                    continue;
                }
                repository.history_add(
                    &conversation,
                    generation.job,
                    HistorySelector {
                        id,
                        clock,
                        source_kind: kind,
                        ordinal: index as i64,
                        priority,
                        sequence,
                        charged_bytes: plain.len() * 3 + if kind == 4 { 512 } else { 32 * 1024 },
                    },
                    deadline,
                )?;
            }
            while !generation.ready {
                generation = repository.history_order_step(generation.job)?;
            }
        }
        let (start, end, newer) = match selection {
            HistorySelection::Recent(limit) => {
                (generation.count.saturating_sub(limit), generation.count, 0)
            }
            HistorySelection::Selected(_) => (0, 0, 0),
            HistorySelection::Context(id) | HistorySelection::Protected(id) => {
                let Some(rank) = repository.history_rank(generation.job, id)? else {
                    return Ok(None);
                };
                if matches!(selection, HistorySelection::Protected(_)) {
                    (rank, rank + 1, generation.count - rank - 1)
                } else {
                    (
                        rank.saturating_sub(25),
                        (rank + 26).min(generation.count),
                        generation.count - rank - 1,
                    )
                }
            }
        };
        let mut selectors = Vec::new();
        let mut budget = 8 * 1024 * 1024usize;
        if let HistorySelection::Selected(ids) = selection {
            for id in ids {
                if let Some(rank) = repository.history_rank(generation.job, *id)? {
                    let page =
                        repository.history_selectors(generation.job, rank, rank + 1, budget)?;
                    for row in page {
                        budget -= row.charged_bytes;
                        selectors.push((rank, row));
                    }
                }
            }
            selectors.sort_by_key(|(rank, _)| *rank);
            selectors.dedup_by_key(|(_, row)| row.id);
        } else {
            let mut cursor = start;
            while cursor < end {
                let page = repository.history_selectors(
                    generation.job,
                    cursor,
                    (cursor + 128).min(end),
                    budget,
                )?;
                for row in page {
                    budget -= row.charged_bytes;
                    selectors.push((cursor, row));
                    cursor += 1;
                }
            }
        }
        let mut messages = Vec::new();
        let mut authors = Vec::new();
        for (_, selector) in selectors {
            let mut author_account = None;
            let (plain, from, file) = match selector.source_kind {
                SENT => {
                    let row = sent.row_by_ordinal(selector.ordinal)?.ok_or_else(|| {
                        crate::eventlog::LogError::CorruptFile("sent selector disappeared".into())
                    })?;
                    (row.plaintext, None, false)
                }
                RECEIVED | FILES => {
                    if selector.source_kind == FILES
                        && !file_book.eligible_ordinal(selector.ordinal)?
                    {
                        continue;
                    }
                    let row = if selector.source_kind == RECEIVED {
                        received.row_by_ordinal(selector.ordinal)?
                    } else {
                        files.row_by_ordinal(selector.ordinal)?
                    }
                    .ok_or_else(|| {
                        crate::eventlog::LogError::CorruptFile(
                            "received selector disappeared".into(),
                        )
                    })?;
                    if selector.source_kind == FILES {
                        author_account = identities.row_author_account(&row)?;
                    }
                    (row.plaintext, Some(row.from), selector.source_kind == FILES)
                }
                _ => match pending
                    .get(usize::try_from(selector.ordinal).map_err(|_| {
                        crate::eventlog::LogError::CorruptFile("invalid pending ordinal".into())
                    })?)
                    .ok_or_else(|| {
                        crate::eventlog::LogError::CorruptFile(
                            "pending history selector disappeared".into(),
                        )
                    })?
                    .body()
                {
                    DeliveryTransaction::Outgoing { sent, .. } => {
                        (sent.plaintext.clone(), None, false)
                    }
                    DeliveryTransaction::OutgoingManifest { received, .. } => (
                        received.plaintext.clone(),
                        Some(received.from.clone()),
                        true,
                    ),
                    DeliveryTransaction::ChannelOutgoing { sent, .. } => {
                        (sent.plaintext.clone(), None, true)
                    }
                    _ => {
                        return Err(crate::eventlog::LogError::CorruptFile(
                            "invalid pending history selector".into(),
                        )
                        .into());
                    }
                },
            };
            let from_me = from.as_ref().is_none_or(|from| {
                if file {
                    from == &me || own_devices.contains(from) || recovered_me.contains(from)
                } else if account.is_some() {
                    from == &my_account
                } else {
                    own_devices.contains(from)
                }
            });
            let envelope = account.and_then(|_| DmEnvelope::decode_ref(&plain));
            let inner = if file {
                plain.as_slice()
            } else {
                envelope.as_ref().map_or(plain.as_slice(), |e| e.body)
            };
            if from.is_some() && self.recipient_expired(inner, selector.id, selector.clock) {
                continue;
            }
            let row = HistoryRow {
                id: selector.id,
                wall_clock: selector.clock,
                from_me,
                who: if from_me {
                    "you"
                } else {
                    from.as_deref().unwrap_or("you")
                },
                plaintext: inner,
                expires_at_ms: super::expiry_policy::plaintext_deadline(&plain),
                file,
            };
            let mut entry = row.decode();
            entry.reply_to = entry
                .reply_to
                .map(|target| identities.logical(conversation, target))
                .transpose()?;
            if file {
                entry.file_message_aliases = identities.aliases(conversation, selector.id)?;
            }
            if matches!(selection,HistorySelection::Protected(id) if id==selector.id)
                && entry.protection.is_some()
                && !file
            {
                entry.text = MessageBody::decode_revealed(inner).text;
            }
            authors.push(if from_me {
                Some(my_account.clone())
            } else if file {
                author_account
            } else if account.is_some() {
                from.clone()
            } else {
                None
            });
            messages.push(entry);
        }
        drop(suppression);
        drop(identities);
        drop(files);
        drop(received);
        drop(recovered);
        drop(sent);
        drop(log);
        drop(delivery);
        let mut recalled = HashSet::new();
        for (entry, author) in messages.iter().zip(&authors) {
            if self.message_recalled_selected(conversation, entry.id, account, author.as_deref())? {
                recalled.insert(entry.id);
            }
        }
        if matches!(selection,HistorySelection::Context(id)|HistorySelection::Protected(id) if recalled.contains(&id))
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
            newer_entries: newer,
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

#[cfg(test)]
pub(in crate::node) fn decode_account_entry(plaintext: &[u8]) -> (EventId, MessageBody) {
    match DmEnvelope::decode(plaintext) {
        Some(env) => (EventId::new(env.msg_id), MessageBody::decode(&env.body)),
        None => (EventId::new([0u8; 32]), MessageBody::decode(plaintext)),
    }
}
