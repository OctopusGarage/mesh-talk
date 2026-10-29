//! Local message lifecycle: delete a single message, clear a conversation, and prune by
//! age (retention). Split out of `node.rs` (one `impl Node` block per domain).
//!
//! These are LOCAL erases — they rewrite only the private plaintext sidecars
//! ([`SentLog`], [`ReceivedLog`], and the `received_files` store), never the synced event
//! log. The synced log keeps only ciphertext, deduped by event id, so a purged received
//! message is not re-ingested on a later sync, and DM plaintext can't be re-derived (the
//! ratchet key is single-use). The erased content is therefore genuinely gone on this
//! device and cannot be resurrected. Recall (which DOES propagate to peers) lives
//! separately in `recall.rs`.

use super::node::Node;
use crate::eventlog::event::{ConversationId, EventId, EventKind};
use crate::node::NodeError;
use crate::node::conversation::account_conversation_id;

impl Node {
    /// Delete one message in the account conversation with `peer_account_id` (resolves the
    /// conversation id, then [`Self::delete_message`] in the account id space).
    pub fn delete_account_message(
        &self,
        peer_account_id: &str,
        target: EventId,
    ) -> Result<usize, NodeError> {
        let conv = account_conversation_id(&self.account.account_id(), peer_account_id);
        self.delete_message(conv, target, true)
    }

    /// Clear the account conversation with `peer_account_id` (resolves the conversation id).
    pub fn clear_account_conversation(&self, peer_account_id: &str) -> Result<usize, NodeError> {
        let conv = account_conversation_id(&self.account.account_id(), peer_account_id);
        self.clear_conversation(conv)
    }
}

impl Node {
    /// Delete one message from THIS device only (no placeholder, not propagated). `target`
    /// is the logical message id as the UI knows it: for an account conversation that's the
    /// `DmEnvelope` msg id, otherwise the event id. `account` selects which id space to match.
    /// Returns how many sidecar entries were removed (0 if the message wasn't found locally).
    pub fn delete_message(
        &self,
        conv: ConversationId,
        target: EventId,
        account: bool,
    ) -> Result<usize, NodeError> {
        use super::transcript_repository::TranscriptPageLimit;
        let target = self.file_message_logical_id(conv, target)?;
        let suppress = account
            || self
                .list_channels()?
                .iter()
                .any(|channel| channel.id == conv);
        let mut delivery = self.delivery.lock().expect("delivery mutex");
        self.recover_delivery(&mut delivery)?;
        let mut aliases = self
            .file_message_identities
            .lock()
            .expect("file identity mutex")
            .aliases(conv, target)?;
        if !aliases.contains(&target) {
            aliases.push(target);
        }
        let mut file_ids = Vec::new();
        let mut media = Vec::new();
        for id in &aliases {
            let row = self
                .received_files
                .lock()
                .expect("files mutex")
                .entry_in(&conv, *id)?;
            if let Some(row) = row
                && self.file_message_row_logical_id(&row)? == target
            {
                file_ids.push(row.event_id);
                if let Some(manifest) = super::files::validated_manifest(&row.plaintext) {
                    media.push(manifest.file_conv());
                }
            }
        }
        delivery.check_file_erasure_capacity(file_ids.iter().map(|id| (conv, *id)))?;
        if suppress {
            self.account_suppression
                .lock()
                .expect("suppression mutex")
                .suppress(conv, target)?;
        }
        for id in &file_ids {
            self.account_suppression
                .lock()
                .expect("suppression mutex")
                .suppress(conv, *id)?;
        }
        self.my_dm_reactions
            .lock()
            .expect("reactions mutex")
            .remove_where(|c, _, payload| {
                c == conv && (payload.target == target || aliases.contains(&payload.target))
            })?;
        if delivery.cancel(conv, target)? {
            self.privacy
                .generation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        for id in &file_ids {
            if delivery.erase_file(conv, *id)? {
                self.privacy
                    .generation
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        self.delivery_control_ids
            .lock()
            .expect("control ids mutex")
            .replace(delivery.control_ids());
        // Once erasure facts commit, revoke file keys even if physical row
        // removal later fails and needs a retry.
        self.reseed_live_file_book(&delivery)?;
        let mut removed = 0;
        for kind in [
            super::transcript_repository::RECEIVED,
            super::transcript_repository::FILES,
            super::transcript_repository::SENT,
        ] {
            let mut after = None;
            loop {
                let page = match kind {
                    super::transcript_repository::SENT => self
                        .sentlog
                        .lock()
                        .expect("sent mutex")
                        .header_scan_page(Some(&conv), after, TranscriptPageLimit::default())?,
                    super::transcript_repository::RECEIVED => self
                        .received
                        .lock()
                        .expect("received mutex")
                        .header_scan_page(Some(&conv), after, TranscriptPageLimit::default())?,
                    _ => self
                        .received_files
                        .lock()
                        .expect("files mutex")
                        .header_scan_page(Some(&conv), after, TranscriptPageLimit::default())?,
                };
                if page.rows.is_empty() {
                    break;
                }
                let mut ordinals = Vec::new();
                for header in page.rows {
                    after = Some(header.ordinal);
                    let id = if kind == super::transcript_repository::SENT {
                        if account {
                            header.logical_id.unwrap_or(EventId::new([0; 32]))
                        } else {
                            self.sent_header_identity(&header)?
                        }
                    } else {
                        let original =
                            EventId::new(header.key.as_slice().try_into().map_err(|_| {
                                crate::eventlog::LogError::CorruptFile(
                                    "invalid received identity".into(),
                                )
                            })?);
                        if kind == super::transcript_repository::FILES {
                            if file_ids.contains(&original) {
                                target
                            } else {
                                continue;
                            }
                        } else if account {
                            header.logical_id.unwrap_or(original)
                        } else {
                            original
                        }
                    };
                    if id == target {
                        ordinals.push(header.ordinal);
                    }
                }
                removed += match kind {
                    super::transcript_repository::SENT => self
                        .sentlog
                        .lock()
                        .expect("sent mutex")
                        .remove_ordinals(&ordinals)?,
                    super::transcript_repository::RECEIVED => self
                        .received
                        .lock()
                        .expect("received mutex")
                        .remove_ordinals(&ordinals)?,
                    _ => self
                        .received_files
                        .lock()
                        .expect("files mutex")
                        .remove_ordinals(&ordinals)?,
                };
            }
        }
        self.remove_unreferenced_media(media)?;
        Ok(removed)
    }

    /// Clear all locally-stored history for `conversation` (text + file messages, both
    /// directions) from this device. Returns how many sidecar entries were removed.
    pub fn clear_conversation(&self, conversation: ConversationId) -> Result<usize, NodeError> {
        self.remove_matching(|c, _| c == conversation)
    }

    /// Retention prune: erase every locally-stored message older than `cutoff_ms`
    /// (`wall_clock < cutoff_ms`) across all conversations. Returns how many entries were
    /// removed. The caller derives the cutoff from the retention setting (now − N days); a
    /// cutoff of 0 removes nothing.
    pub fn prune_older_than(&self, cutoff_ms: u64) -> Result<usize, NodeError> {
        self.remove_matching(|_, wall_clock| wall_clock < cutoff_ms)
    }

    /// Shared erase across all three plaintext sidecars (sent, received, received-files),
    /// keeping only entries for which `keep_removes(conversation, wall_clock)` is false.
    fn remove_matching(
        &self,
        should_remove: impl Fn(ConversationId, u64) -> bool,
    ) -> Result<usize, NodeError> {
        use super::transcript_repository::{FILES, RECEIVED, SENT, TranscriptPageLimit};
        let channels: std::collections::HashSet<_> =
            self.list_channels()?.into_iter().map(|c| c.id).collect();
        let mut delivery = self.delivery.lock().expect("delivery mutex");
        self.recover_delivery(&mut delivery)?;
        let mut after = None;
        let mut additional = 0;
        loop {
            let page = self
                .received_files
                .lock()
                .expect("files mutex")
                .header_scan_page(None, after, TranscriptPageLimit::default())?;
            if page.rows.is_empty() {
                break;
            }
            for h in page.rows {
                after = Some(h.ordinal);
                let id = EventId::new(h.key.as_slice().try_into().map_err(|_| {
                    crate::eventlog::LogError::CorruptFile("invalid file identity".into())
                })?);
                if should_remove(h.conversation, h.wall_clock)
                    && delivery.file_erasure_needs_slot(h.conversation, id)
                {
                    additional += 1;
                    delivery.check_file_erasure_count(additional)?;
                }
            }
        }
        // Suppress signed content even when a crash left no plaintext projection.
        for channel in &channels {
            let log = self.log.lock().expect("log mutex");
            let mut cursor = None;
            loop {
                let page = log.repository()?.headers_page(
                    channel,
                    cursor.as_ref(),
                    crate::eventlog::repository::pages::PageBudget {
                        rows: 128,
                        bytes: 65536,
                    },
                )?;
                for header in page.items {
                    if matches!(header.kind, EventKind::Message | EventKind::FileManifest)
                        && should_remove(*channel, header.wall_clock)
                    {
                        self.account_suppression
                            .lock()
                            .expect("suppression mutex")
                            .suppress(*channel, header.id)?;
                    }
                }
                cursor = page.next;
                if cursor.is_none() {
                    break;
                }
            }
        }
        let mut removed = 0;
        for kind in [SENT, RECEIVED, FILES] {
            let mut after = None;
            loop {
                let page = match kind {
                    SENT => self.sentlog.lock().expect("sent mutex").header_scan_page(
                        None,
                        after,
                        TranscriptPageLimit::default(),
                    )?,
                    RECEIVED => self
                        .received
                        .lock()
                        .expect("received mutex")
                        .header_scan_page(None, after, TranscriptPageLimit::default())?,
                    _ => self
                        .received_files
                        .lock()
                        .expect("files mutex")
                        .header_scan_page(None, after, TranscriptPageLimit::default())?,
                };
                if page.rows.is_empty() {
                    break;
                }
                let mut ordinals = Vec::new();
                let mut media = Vec::new();
                for h in page.rows {
                    after = Some(h.ordinal);
                    if !should_remove(h.conversation, h.wall_clock) {
                        continue;
                    }
                    let id = if kind == SENT {
                        h.logical_id.or(if channels.contains(&h.conversation) {
                            Some(self.sent_header_identity(&h)?)
                        } else {
                            None
                        })
                    } else {
                        Some(EventId::new(h.key.as_slice().try_into().map_err(|_| {
                            crate::eventlog::LogError::CorruptFile(
                                "invalid transcript identity".into(),
                            )
                        })?))
                    };
                    if let Some(id) = id {
                        if kind == FILES
                            || channels.contains(&h.conversation)
                            || h.logical_id.is_some()
                        {
                            self.account_suppression
                                .lock()
                                .expect("suppression mutex")
                                .suppress(h.conversation, h.logical_id.unwrap_or(id))?;
                        }
                        if kind == FILES {
                            let logical = self
                                .file_message_identities
                                .lock()
                                .expect("file identity mutex")
                                .logical_for_ordinal(h.ordinal)?
                                .ok_or_else(|| {
                                    crate::eventlog::LogError::CorruptFile(
                                        "missing file selector".into(),
                                    )
                                })?;
                            self.account_suppression
                                .lock()
                                .expect("suppression mutex")
                                .suppress(h.conversation, logical)?;
                            if delivery.erase_file(h.conversation, id)? {
                                self.privacy
                                    .generation
                                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            }
                            if let Some(file) = h.file_conversation {
                                media.push(file);
                            }
                        }
                    }
                    ordinals.push(h.ordinal);
                }
                removed += match kind {
                    SENT => self
                        .sentlog
                        .lock()
                        .expect("sent mutex")
                        .remove_ordinals(&ordinals)?,
                    RECEIVED => self
                        .received
                        .lock()
                        .expect("received mutex")
                        .remove_ordinals(&ordinals)?,
                    _ => self
                        .received_files
                        .lock()
                        .expect("files mutex")
                        .remove_ordinals(&ordinals)?,
                };
                self.remove_unreferenced_media(media)?;
            }
        }
        self.my_dm_reactions
            .lock()
            .expect("reactions mutex")
            .try_remove_where(|conv, wall, payload| {
                if should_remove(conv, wall) {
                    return Ok(true);
                }
                self.account_suppression
                    .lock()
                    .expect("suppression mutex")
                    .contains(conv, payload.target)
            })?;
        for (conv, id, clock) in delivery.cancellation_rows() {
            if should_remove(conv, clock) && delivery.cancel(conv, id)? {
                self.privacy
                    .generation
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        self.delivery_control_ids
            .lock()
            .expect("control ids mutex")
            .replace(delivery.control_ids());
        self.reseed_live_file_book(&delivery)?;
        Ok(removed)
    }

    pub(in crate::node) fn remove_unreferenced_media(
        &self,
        candidates: Vec<ConversationId>,
    ) -> Result<(), NodeError> {
        if candidates.is_empty() {
            return Ok(());
        }
        for file_conv in candidates {
            let mut after = None;
            let mut live = false;
            loop {
                let page = self
                    .received_files
                    .lock()
                    .expect("files mutex")
                    .file_headers(
                        &file_conv,
                        after,
                        super::transcript_repository::TranscriptPageLimit::default(),
                    )?;
                if page.rows.is_empty() {
                    break;
                }
                for header in page.rows {
                    after = Some(header.ordinal);
                    let row = self
                        .received_files
                        .lock()
                        .expect("files mutex")
                        .row_by_ordinal(header.ordinal)?;
                    if let Some(row) = row
                        && !self.file_message_row_suppressed(&row)?
                    {
                        live = true;
                        break;
                    }
                }
                if live {
                    break;
                }
            }
            if !live {
                self.media
                    .remove(file_conv)
                    .map_err(|e| NodeError::Log(e.into()))?;
            }
        }

        Ok(())
    }
}

/// The logical id of a received plaintext entry: the `DmEnvelope` msg id for an account
/// conversation, otherwise the event id (matching how `history`/`account_history` derive
/// the id the UI targets).
fn received_logical_id(plaintext: &[u8], event_id: EventId, account: bool) -> EventId {
    if account {
        super::queries::decode_account_entry(plaintext).0
    } else {
        event_id
    }
}
