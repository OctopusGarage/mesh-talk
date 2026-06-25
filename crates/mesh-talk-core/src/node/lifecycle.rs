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
use crate::node::conversation::account_conversation_id;
use crate::node::NodeError;
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

        // File/media messages (their own ReceivedLog) are always keyed by event id.
        removed += self
            .received_files
            .lock()
            .expect("received_files mutex not poisoned")
            .remove_where(|e| e.conversation == conv && e.event_id == target)
            .map_err(NodeError::Log)?;

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
                    seq_to_id
                        .as_ref()
                        .and_then(|m| m.get(&e.seq).copied())
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
        removed += self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .remove_where(|e| should_remove(e.conversation, e.wall_clock))
            .map_err(NodeError::Log)?;
        Ok(removed)
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
