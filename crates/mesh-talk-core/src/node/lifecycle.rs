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
use super::queries::decode_account_entry;
use crate::eventlog::event::{Author, ConversationId, EventId, EventKind};
use crate::node::NodeError;
use crate::node::conversation::account_conversation_id;
use std::collections::HashMap;

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
        let target = self.file_message_logical_id(conv, target);
        let suppress = account
            || self
                .list_channels()
                .iter()
                .any(|channel| channel.id == conv);
        let mut delivery = self.delivery.lock().expect("delivery lock not poisoned");
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        let file_rows = self
            .received_files
            .lock()
            .expect("files lock not poisoned")
            .entries_ref(&conv)
            .iter()
            .filter(|row| self.file_message_row_logical_id(row) == target)
            .cloned()
            .collect::<Vec<_>>();
        delivery
            .check_file_erasure_capacity(
                file_rows.iter().map(|row| (row.conversation, row.event_id)),
            )
            .map_err(NodeError::Log)?;
        if suppress {
            self.account_suppression
                .lock()
                .expect("account suppression mutex not poisoned")
                .suppress(conv, target)
                .map_err(NodeError::Log)?;
        }
        self.my_dm_reactions
            .lock()
            .expect("my_dm_reactions mutex not poisoned")
            .remove_where(|c, _, payload| {
                c == conv && self.file_message_logical_id(c, payload.target) == target
            })
            .map_err(NodeError::Log)?;
        if delivery.cancel(conv, target).map_err(NodeError::Log)? {
            self.privacy
                .generation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        *self
            .delivery_control_ids
            .lock()
            .expect("control ids lock not poisoned") = delivery.control_ids();
        let mut removed = 0;

        // Received plaintext sidecar: match by logical id.
        removed += self
            .received
            .lock()
            .expect("received mutex not poisoned")
            .remove_where(|e| {
                e.conversation == conv
                    && received_logical_id(&e.plaintext, e.event_id, account) == target
            })
            .map_err(NodeError::Log)?;

        // Resolve authenticated device aliases while retaining exact offer erasure.

        let file_ids: std::collections::HashSet<_> =
            file_rows.iter().map(|row| row.event_id).collect();
        {
            let mut suppression = self
                .account_suppression
                .lock()
                .expect("account suppression lock");
            for id in &file_ids {
                suppression.suppress(conv, *id).map_err(NodeError::Log)?;
            }
        }
        let media_to_remove: Vec<_> = file_rows
            .iter()
            .filter_map(|row| {
                super::files::validated_manifest(&row.plaintext)
                    .map(|manifest| manifest.file_conv())
            })
            .collect();
        let erased = (|| -> Result<(), NodeError> {
            for row in file_rows {
                if delivery
                    .erase_file(row.conversation, row.event_id)
                    .map_err(NodeError::Log)?
                {
                    self.privacy
                        .generation
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    *self
                        .delivery_control_ids
                        .lock()
                        .expect("control ids lock not poisoned") = delivery.control_ids();
                }
            }
            Ok(())
        })();
        self.reseed_live_file_book(&delivery);
        erased?;
        removed += self
            .received_files
            .lock()
            .expect("received_files mutex not poisoned")
            .remove_where(|e| e.conversation == conv && file_ids.contains(&e.event_id))
            .map_err(NodeError::Log)?;
        self.reseed_live_file_book(&delivery);
        self.remove_unreferenced_media(media_to_remove)?;

        // Sent plaintext sidecar: account entries carry the msg id in their envelope; plain
        // DM/channel sends are matched via the event log's seq -> id map (built in its own
        // scope so the log lock is released before the sentlog lock).
        let seq_to_id: Option<HashMap<u64, EventId>> = if account {
            None
        } else {
            let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
            let log = self.log.lock().expect("log mutex not poisoned");
            let mut m = HashMap::new();
            for event in log.events(&conv) {
                if event.kind == EventKind::Message && event.author == self_author {
                    m.insert(event.seq, event.id);
                }
            }
            Some(m)
        };
        let recovered_ids = self
            .recovered_sent_ids
            .lock()
            .expect("recovered sent ids mutex not poisoned");
        removed += self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .remove_where(|e| {
                if e.conversation != conv {
                    return false;
                }
                let id = if account {
                    decode_account_entry(&e.plaintext).0
                } else {
                    recovered_ids
                        .as_ref()
                        .and_then(|saved| saved.id_for(e))
                        .or_else(|| seq_to_id.as_ref().and_then(|m| m.get(&e.seq).copied()))
                        .unwrap_or(EventId::new([0u8; 32]))
                };
                id == target
            })
            .map_err(NodeError::Log)?;

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
        let channels: std::collections::HashSet<_> = self
            .list_channels()
            .into_iter()
            .map(|channel| channel.id)
            .collect();
        // raw_sent_history acquires delivery itself; collect only its IDs first.
        let mut suppressed = Vec::new();
        for channel in &channels {
            for (id, entry) in self.raw_sent_history(*channel) {
                if should_remove(*channel, entry.wall_clock) {
                    suppressed.push((*channel, id));
                }
            }
        }
        let mut delivery = self.delivery.lock().expect("delivery lock not poisoned");
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        let file_rows = {
            let files = self.received_files.lock().expect("files lock not poisoned");
            files
                .conversations()
                .iter()
                .flat_map(|host| files.entries_ref(host))
                .filter(|row| should_remove(row.conversation, row.wall_clock))
                .cloned()
                .collect::<Vec<_>>()
        };
        delivery
            .check_file_erasure_capacity(
                file_rows.iter().map(|row| (row.conversation, row.event_id)),
            )
            .map_err(NodeError::Log)?;

        {
            let received = self.received.lock().expect("received mutex not poisoned");
            for conv in received.conversations() {
                for entry in received.entries_ref(&conv) {
                    if should_remove(conv, entry.wall_clock) {
                        if channels.contains(&conv) {
                            suppressed.push((conv, entry.event_id));
                        } else if let Some(envelope) =
                            super::dm_envelope::DmEnvelope::decode(&entry.plaintext)
                        {
                            suppressed.push((conv, EventId::new(envelope.msg_id)));
                        }
                    }
                }
            }
        }
        {
            let sent = self.sentlog.lock().expect("sentlog mutex not poisoned");
            for conv in sent.conversations() {
                for entry in sent.entries_ref(&conv) {
                    if should_remove(conv, entry.wall_clock)
                        && let Some(envelope) =
                            super::dm_envelope::DmEnvelope::decode(&entry.plaintext)
                    {
                        suppressed.push((conv, EventId::new(envelope.msg_id)));
                    }
                }
            }
        }
        for channel in channels {
            // A failed projection may have no plaintext row yet. Suppress known
            // signed content IDs too, so pending retries and startup repair cannot
            // undo a local clear or retention prune. No pending-queue lock is needed.
            {
                let log = self.log.lock().expect("log lock not poisoned");
                for event in log.events(&channel) {
                    if matches!(event.kind, EventKind::Message | EventKind::FileManifest)
                        && should_remove(channel, event.wall_clock)
                    {
                        suppressed.push((channel, event.id));
                    }
                }
            }
        }
        {
            let files = self.received_files.lock().expect("files lock not poisoned");
            for conv in files.conversations() {
                for entry in files.entries_ref(&conv) {
                    if should_remove(conv, entry.wall_clock) {
                        suppressed.push((conv, entry.event_id));
                        suppressed.push((conv, self.file_message_row_logical_id(entry)));
                    }
                }
            }
        }
        self.my_dm_reactions
            .lock()
            .expect("my_dm_reactions mutex not poisoned")
            .remove_where(|conv, wall, payload| {
                should_remove(conv, wall) || suppressed.contains(&(conv, payload.target))
            })
            .map_err(NodeError::Log)?;
        {
            let mut suppression = self
                .account_suppression
                .lock()
                .expect("account suppression mutex not poisoned");
            for (conv, id) in suppressed {
                suppression.suppress(conv, id).map_err(NodeError::Log)?;
            }
        }
        let cancelled: Result<(), NodeError> = (|| {
            for (conversation, id, clock) in delivery.cancellation_rows() {
                if should_remove(conversation, clock)
                    && delivery.cancel(conversation, id).map_err(NodeError::Log)?
                {
                    // Invalidate subsequent frame admission even if a later
                    // cancellation fails. Already-started writes are not recall.
                    self.privacy
                        .generation
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }
            Ok(())
        })();
        *self
            .delivery_control_ids
            .lock()
            .expect("control ids lock not poisoned") = delivery.control_ids();
        cancelled?;
        let media_to_remove: Vec<_> = file_rows
            .iter()
            .filter_map(|row| {
                super::files::validated_manifest(&row.plaintext)
                    .map(|manifest| manifest.file_conv())
            })
            .collect();
        let erased = (|| -> Result<(), NodeError> {
            for row in file_rows {
                if delivery
                    .erase_file(row.conversation, row.event_id)
                    .map_err(NodeError::Log)?
                {
                    self.privacy
                        .generation
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    *self
                        .delivery_control_ids
                        .lock()
                        .expect("control ids lock not poisoned") = delivery.control_ids();
                }
            }
            Ok(())
        })();
        self.reseed_live_file_book(&delivery);
        erased?;
        let mut removed = 0;
        removed += self
            .received
            .lock()
            .expect("received mutex not poisoned")
            .remove_where(|e| should_remove(e.conversation, e.wall_clock))
            .map_err(NodeError::Log)?;

        removed += self
            .received_files
            .lock()
            .expect("received_files mutex not poisoned")
            .remove_where(|e| should_remove(e.conversation, e.wall_clock))
            .map_err(NodeError::Log)?;
        self.reseed_live_file_book(&delivery);
        self.remove_unreferenced_media(media_to_remove)?;
        removed += self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .remove_where(|e| should_remove(e.conversation, e.wall_clock))
            .map_err(NodeError::Log)?;
        Ok(removed)
    }

    pub(in crate::node) fn remove_unreferenced_media(
        &self,
        candidates: Vec<ConversationId>,
    ) -> Result<(), NodeError> {
        if candidates.is_empty() {
            return Ok(());
        }
        let records = self.received_files.lock().expect("files lock not poisoned");
        let remaining: std::collections::HashSet<_> = records
            .conversations()
            .iter()
            .flat_map(|conversation| records.entries(conversation))
            .filter(|row| !self.file_message_row_suppressed(row))
            .filter_map(|row| {
                super::files::validated_manifest(&row.plaintext)
                    .map(|manifest| manifest.file_conv())
            })
            .collect();
        drop(records);
        for file_conv in candidates {
            if !remaining.contains(&file_conv) {
                self.media
                    .remove(file_conv)
                    .map_err(|e| NodeError::File(format!("remove media: {e}")))?;
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
        decode_account_entry(plaintext).0
    } else {
        event_id
    }
}
