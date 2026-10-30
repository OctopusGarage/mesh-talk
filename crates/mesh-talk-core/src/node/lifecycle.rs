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
use std::collections::{HashMap, HashSet};

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

    /// Erase an older personal-deletion scope while preserving content whose
    /// signed ancestry proves it followed a currently authorized fresh lifetime.
    /// The caller holds the membership guard and validates these current lifetimes.
    pub(in crate::node) fn clear_channel_conversation_preserving_lifetimes(
        &self,
        conversation: ConversationId,
        fresh: &[super::account_group::MembershipLifetime],
    ) -> Result<usize, NodeError> {
        let mut delivery = self.delivery.lock().expect("delivery lock not poisoned");
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        // Capture existing orphaned projections before the public cut. New rows
        // arriving after this snapshot must never be mistaken for old history.
        let mut remove: HashSet<_> = self
            .raw_sent_history_with_store(conversation, &delivery)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        remove.extend(
            self.received
                .lock()
                .expect("received mutex")
                .entries_ref(&conversation)
                .iter()
                .map(|row| row.event_id),
        );
        remove.extend(
            self.received_files
                .lock()
                .expect("files mutex")
                .entries_ref(&conversation)
                .iter()
                .map(|row| row.event_id),
        );
        {
            let log = self.log.lock().expect("log mutex not poisoned");
            let mut pending: Vec<_> = fresh
                .iter()
                .map(|lifetime| lifetime.acceptance.unwrap_or(lifetime.membership))
                .filter(|id| {
                    log.get(id)
                        .is_some_and(|event| event.conversation_id == conversation)
                })
                .collect();
            let events = log.events(&conversation);
            let mut children: HashMap<EventId, Vec<EventId>> = HashMap::new();
            for event in &events {
                if matches!(event.kind, EventKind::Message | EventKind::FileManifest) {
                    remove.insert(event.id);
                }
                for parent in &event.parents {
                    children.entry(*parent).or_default().push(event.id);
                }
            }
            // Follow actual signed edges: remote Lamport timestamps need not
            // be honest, and unrelated wall clocks never establish ancestry.
            let mut protected = HashSet::new();
            while let Some(id) = pending.pop() {
                if protected.insert(id)
                    && let Some(children) = children.get(&id)
                {
                    pending.extend(children);
                }
            }
            remove.retain(|id| !protected.contains(id));
        }
        self.remove_matching_events_with_store(
            |c, _, id| c == conversation && id.is_some_and(|id| remove.contains(&id)),
            &mut delivery,
        )
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
        let mut delivery = self.delivery.lock().expect("delivery lock not poisoned");
        self.remove_matching_with_store(should_remove, &mut delivery)
    }
    pub(in crate::node) fn clear_account_conversation_with_store(
        &self,
        peer: &str,
        delivery: &mut super::delivery_store::DeliveryStore,
    ) -> Result<usize, NodeError> {
        let conversation = account_conversation_id(&self.account_id(), peer);
        self.remove_matching_with_store(|c, _| c == conversation, delivery)
    }
    fn remove_matching_with_store(
        &self,
        should_remove: impl Fn(ConversationId, u64) -> bool,
        delivery: &mut super::delivery_store::DeliveryStore,
    ) -> Result<usize, NodeError> {
        self.remove_matching_events_with_store(|c, wall, _| should_remove(c, wall), delivery)
    }
    fn remove_matching_events_with_store(
        &self,
        should_remove: impl Fn(ConversationId, u64, Option<EventId>) -> bool,
        delivery: &mut super::delivery_store::DeliveryStore,
    ) -> Result<usize, NodeError> {
        self.recover_delivery(delivery).map_err(NodeError::Log)?;
        let channels: std::collections::HashSet<_> = self
            .list_channels()
            .into_iter()
            .map(|channel| channel.id)
            .collect();
        // Reuse the caller's delivery owner when pending personal cleanup must
        // finish before accepting a new live message.
        let mut suppressed = Vec::new();
        let mut sent_ids = HashMap::new();
        for channel in &channels {
            for (id, entry) in self.raw_sent_history_with_store(*channel, delivery) {
                sent_ids.insert((*channel, entry.seq), id);
                if should_remove(*channel, entry.wall_clock, Some(id)) {
                    suppressed.push((*channel, id));
                }
            }
        }
        let file_rows = {
            let files = self.received_files.lock().expect("files lock not poisoned");
            files
                .conversations()
                .iter()
                .flat_map(|host| files.entries_ref(host))
                .filter(|row| should_remove(row.conversation, row.wall_clock, Some(row.event_id)))
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
                    if should_remove(conv, entry.wall_clock, Some(entry.event_id)) {
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
                    if let Some(envelope) = super::dm_envelope::DmEnvelope::decode(&entry.plaintext)
                        && should_remove(
                            conv,
                            entry.wall_clock,
                            Some(EventId::new(envelope.msg_id)),
                        )
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
                        && should_remove(channel, event.wall_clock, Some(event.id))
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
                    if should_remove(conv, entry.wall_clock, Some(entry.event_id)) {
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
                should_remove(conv, wall, Some(payload.target))
                    || suppressed.contains(&(conv, payload.target))
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
                if should_remove(conversation, clock, Some(id))
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
        self.reseed_live_file_book(delivery);
        erased?;
        let mut removed = 0;
        removed += self
            .received
            .lock()
            .expect("received mutex not poisoned")
            .remove_where(|e| should_remove(e.conversation, e.wall_clock, Some(e.event_id)))
            .map_err(NodeError::Log)?;

        removed += self
            .received_files
            .lock()
            .expect("received_files mutex not poisoned")
            .remove_where(|e| should_remove(e.conversation, e.wall_clock, Some(e.event_id)))
            .map_err(NodeError::Log)?;
        self.reseed_live_file_book(delivery);
        self.remove_unreferenced_media(media_to_remove)?;
        removed += self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .remove_where(|e| {
                let id = sent_ids.get(&(e.conversation, e.seq)).copied().or_else(|| {
                    super::dm_envelope::DmEnvelope::decode(&e.plaintext)
                        .map(|envelope| EventId::new(envelope.msg_id))
                });
                should_remove(e.conversation, e.wall_clock, id)
            })
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

#[cfg(test)]
mod personal_scope_tests {
    use super::*;
    use crate::{
        channel::ChannelMeta,
        discovery::Roster,
        eventlog::Event,
        identity::{account::Account, device::DeviceIdentity},
        node::account_group::AccountGroupRole,
    };
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn late_group_clear_preserves_only_fresh_lifetime_ancestry() {
        let dir = tempfile::tempdir().unwrap();
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (channels, _) = tokio::sync::mpsc::unbounded_channel();
        let (files, _) = tokio::sync::mpsc::unbounded_channel();
        let node = Node::open_with_account(
            DeviceIdentity::generate(),
            Account::generate(),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channels,
            files,
            &dir.path().join("events.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        let owner = DeviceIdentity::generate();
        let group = ConversationId::new([42; 32]);
        let meta = |epoch| ChannelMeta {
            name: "Scope proof".into(),
            members: vec![owner.public(), node.identity.public()],
            epoch,
            owner: owner.public().user_id(),
        };
        let genesis = Event::new(
            &owner,
            group,
            1,
            vec![],
            1,
            10,
            EventKind::MembershipChange,
            meta(0).encode(),
        );
        let old = Event::new(
            &node.identity,
            group,
            1,
            vec![genesis.id],
            2,
            10,
            EventKind::Message,
            vec![1],
        );
        let leave = Event::new(
            &node.identity,
            group,
            2,
            vec![old.id],
            3,
            10,
            EventKind::ChannelLeave,
            vec![],
        );
        let rejoin = Event::new(
            &owner,
            group,
            2,
            vec![leave.id],
            4,
            10,
            EventKind::MembershipChange,
            meta(2).encode_invitation(),
        );
        let accept = Event::new(
            &node.identity,
            group,
            3,
            vec![rejoin.id],
            5,
            10,
            EventKind::ChannelAccept,
            vec![],
        );
        let fresh = Event::new(
            &node.identity,
            group,
            4,
            vec![accept.id],
            6,
            10,
            EventKind::Message,
            vec![2],
        );
        // This late-arriving old branch has a larger wall clock, but no fresh
        // membership ancestor. Wall clocks are never scope evidence.
        let stale_branch = Event::new(
            &owner,
            group,
            3,
            vec![old.id],
            3,
            u64::MAX,
            EventKind::Message,
            vec![3],
        );
        for event in [
            &genesis,
            &old,
            &leave,
            &rejoin,
            &accept,
            &fresh,
            &stale_branch,
        ] {
            node.log
                .lock()
                .unwrap()
                .append_durable(event.clone())
                .unwrap();
        }
        node.process_channel(group);
        let access = node.account_group_access(group).unwrap();
        assert_eq!(access.role, AccountGroupRole::Member);
        let lifetime = access
            .sources
            .iter()
            .find(|source| source.device == node.identity.public())
            .unwrap()
            .lifetime
            .clone();
        assert_eq!(lifetime.membership, rejoin.id);
        assert_eq!(lifetime.acceptance, Some(accept.id));
        for event in [&old, &fresh] {
            node.sentlog
                .lock()
                .unwrap()
                .record(
                    group,
                    event.seq,
                    event.wall_clock,
                    if event.id == old.id { b"old" } else { b"fresh" },
                )
                .unwrap();
        }
        for event in [&stale_branch, &fresh] {
            node.received
                .lock()
                .unwrap()
                .record(
                    group,
                    owner.public().user_id(),
                    event.wall_clock,
                    if event.id == fresh.id {
                        b"fresh received"
                    } else {
                        b"old received"
                    },
                    event.id,
                )
                .unwrap();
        }
        let _membership = node.channel_membership_commit.lock().unwrap();
        node.clear_channel_conversation_preserving_lifetimes(group, &[lifetime])
            .unwrap();
        let sent = node.sentlog.lock().unwrap().entries(&group);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].plaintext, b"fresh");
        let received = node.received.lock().unwrap().entries(&group);
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].event_id, fresh.id);
        let suppression = node.account_suppression.lock().unwrap();
        assert!(suppression.contains(group, old.id));
        assert!(suppression.contains(group, stale_branch.id));
        assert!(!suppression.contains(group, fresh.id));
        drop(suppression);
        drop(_membership);
        let dissolve = Event::new(
            &owner,
            group,
            4,
            vec![fresh.id, stale_branch.id],
            7,
            10,
            EventKind::ChannelDissolve,
            vec![],
        );
        node.log.lock().unwrap().append_durable(dissolve).unwrap();
        node.process_channel(group);
        let closed = node.account_group_access_with_closed(group).unwrap();
        assert_eq!(closed.role, AccountGroupRole::Dissolved);
        assert_eq!(closed.sources.len(), 1);
        assert_eq!(closed.sources[0].lifetime.membership, rejoin.id);
        assert_eq!(closed.sources[0].lifetime.acceptance, Some(accept.id));
        assert!(
            node.account_group_access(group).unwrap().sources.is_empty(),
            "terminal cleanup provenance is never an executable public source"
        );
    }
}
