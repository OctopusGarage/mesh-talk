//! Local verified completion survives reclamation, without extending the wire protocol.
use super::{Node, delivery_store::FileCard};
use crate::{
    eventlog::{ConversationId, EventId, EventKind},
    identity::device::PublicIdentity,
};
use std::path::PathBuf;

/// Sources and sharing receivers retain recent channel ciphertext for members.
/// Count encoded chunk bytes rather than file plaintext, so disk growth stays bounded.
const CHANNEL_FILE_REPLICA_BUDGET: usize = 64 * 1024 * 1024;

fn channel_file_evictions(
    mut replicas: Vec<(crate::eventlog::ConversationId, u64, usize)>,
    budget: usize,
) -> Vec<crate::eventlog::ConversationId> {
    replicas.sort_unstable_by_key(|(file, newest, _)| (*newest, *file.as_bytes()));
    let mut used = 0usize;
    let mut drop = Vec::new();
    for (file, _, bytes) in replicas.into_iter().rev() {
        if used.saturating_add(bytes) <= budget {
            used += bytes;
        } else {
            drop.push(file);
        }
    }
    drop
}

impl Node {
    /// Whether exact incoming sticker offers still need their own completion.
    pub(in crate::node) fn pending_sticker_aliases(&self, conversation: ConversationId) -> bool {
        let sticker = self
            .files
            .lock()
            .expect("files lock not poisoned")
            .manifest(&conversation)
            .is_some_and(|manifest| {
                crate::file::sticker::is_sticker_name(manifest.name())
                    && super::media_store::manifest_is_media(manifest)
                    && manifest.protection().is_none()
            });
        if !sticker || self.expired_received_file(conversation) {
            return false;
        }
        let store = self.delivery.lock().expect("delivery lock not poisoned");
        store.cards_for_file(conversation).iter().any(|card| {
            !store.file_completed(card.id)
                && !store.manifest_event_erased(card.id)
                && card
                    .completion_binding
                    .as_ref()
                    .is_some_and(|binding| binding.owner_account == self.account_id())
        })
    }

    /// Rebind retained original-author completion to an exact compatible sticker offer.
    /// Cached bytes alone never establish signed chunk ancestry or recipient delivery.
    pub(in crate::node) fn complete_sticker_aliases(&self, conversation: ConversationId) -> bool {
        let manifest = {
            let files = self.files.lock().expect("files lock not poisoned");
            let Some(manifest) = files.manifest(&conversation) else {
                return false;
            };
            if !crate::file::sticker::is_sticker_name(manifest.name())
                || !super::media_store::manifest_is_media(manifest)
                || manifest.protection().is_some()
            {
                return false;
            }
            manifest.clone()
        };
        let cards = {
            let store = self.delivery.lock().expect("delivery lock not poisoned");
            store
                .cards_for_file(conversation)
                .into_iter()
                .map(|card| {
                    let completion = store.file_completion(card.id);
                    (card, completion)
                })
                .collect::<Vec<_>>()
        };
        if !cards.iter().any(|(_, completion)| completion.is_none())
            || self.read_media(conversation).is_none()
        {
            return false;
        }
        let rows: std::collections::HashMap<_, _> = self
            .retained_file_rows(conversation)
            .into_iter()
            .map(|row| (row.event_id, row))
            .collect();
        let retained = |card: &FileCard| {
            let binding = card.completion_binding.as_ref()?;
            if binding.owner_account != self.account_id()
                || card.file_conversation != conversation
                || card.chunk_count != manifest.chunk_count()
                || self
                    .delivery
                    .lock()
                    .expect("delivery lock not poisoned")
                    .manifest_event_erased(card.id)
            {
                return None;
            }
            let proof = self.historical_author(&binding.source.ed25519_pub)?;
            if proof.public() != binding.source || proof.account_cert != binding.certificate {
                return None;
            }
            let row = rows.get(&card.id)?;
            let stored = super::files::validated_manifest(&row.plaintext)?;
            let deadline = crate::expiry::deadline(&row.plaintext);
            if row.conversation != card.conversation
                || row.from != binding.source.user_id()
                || stored != manifest
                || !super::files::same_file_transfer(&manifest, &stored)
                || deadline.is_some_and(|time| time <= super::node::now_millis())
            {
                return None;
            }
            let log = self.log.lock().expect("log lock not poisoned");
            let event = log.get(&card.id)?;
            (event.kind == EventKind::FileManifest
                && event.conversation_id
                    == super::conversation::dm_conversation_id(
                        &self.identity.public(),
                        &binding.source,
                    )
                && event.author.ed25519_pub() == &binding.source.ed25519_pub
                && event.verify_integrity()
                && event.verify_signature())
            .then_some(deadline)
        };
        // One authorized completed ancestor per scope is sufficient. Bound new
        // completions per pass and validate retained rows once, not per alias pair.
        let key = |card: &FileCard, deadline| {
            let binding = card.completion_binding.as_ref().expect("validated binding");
            (
                card.conversation,
                deadline,
                binding.source.ed25519_pub,
                binding.source.x25519_pub,
            )
        };
        let mut ancestors = std::collections::HashMap::new();
        for (card, completion) in &cards {
            let Some(final_chunk) = completion else {
                continue;
            };
            let Some(row) = rows.get(&card.id) else {
                continue;
            };
            if card.completion_binding.is_none() {
                continue;
            }
            let scope = key(card, crate::expiry::deadline(&row.plaintext));
            if ancestors.contains_key(&scope) || retained(card).is_none() {
                continue;
            }
            let binding = card.completion_binding.as_ref().expect("validated binding");
            if self.historical_file_completion(conversation, *final_chunk, &binding.source) {
                ancestors.insert(scope, (card, *final_chunk));
            }
        }
        let mut candidates = Vec::new();
        let mut proofs = Vec::new();
        let mut pending: Vec<_> = cards
            .iter()
            .filter(|(_, completion)| completion.is_none())
            .collect();
        let offset = self
            .file_pull_cursor
            .fetch_add(256, std::sync::atomic::Ordering::Relaxed)
            % pending.len();
        pending.rotate_left(offset);
        for (card, _) in pending.into_iter().take(256) {
            let Some(deadline) = retained(card) else {
                continue;
            };
            if let Some((prior, final_chunk)) = ancestors.get(&key(card, deadline))
                && prior.completion_binding == card.completion_binding
            {
                candidates.push((card.clone(), *final_chunk));
                proofs.push((card.id, (*prior).clone(), *final_chunk));
            }
        }
        if candidates.is_empty() {
            return false;
        }
        let completed =
            self.commit_file_completion_with_ancestors(conversation, &candidates, &proofs);
        if completed {
            self.note_file_change();
        }
        completed
    }

    /// Capture exact original-author ancestry before any saved content replaces chunks.
    pub(in crate::node) fn completion_candidates(
        &self,
        conversation: ConversationId,
    ) -> Vec<(FileCard, EventId)> {
        let cards = self
            .delivery
            .lock()
            .expect("delivery lock not poisoned")
            .cards_for_file(conversation);
        let log = self.log.lock().expect("log lock not poisoned");
        let chunks = log.events(&conversation);
        cards
            .into_iter()
            .filter_map(|card| {
                let binding = card.completion_binding.as_ref()?;
                if binding.owner_account != self.account_id()
                    || chunks.len() != card.chunk_count as usize
                {
                    return None;
                }
                for (index, event) in chunks.iter().enumerate() {
                    let parents = if index == 0 {
                        vec![]
                    } else {
                        vec![chunks[index - 1].id]
                    };
                    if event.kind != EventKind::Message
                        || event.seq != index as u64 + 1
                        || event.parents != parents
                        || event.author.ed25519_pub() != &binding.source.ed25519_pub
                        || !event.verify_integrity()
                        || !event.verify_signature()
                    {
                        return None;
                    }
                }
                let final_chunk = chunks.last()?.id;
                Some((card, final_chunk))
            })
            .collect()
    }

    /// Called only after all chunk AEAD/hashes/checksum and saved-content synchronization.
    /// An unavailable legacy reservation retains ciphertext rather than guessing a proof.
    pub(in crate::node) fn commit_file_completion(
        &self,
        conversation: ConversationId,
        candidates: &[(FileCard, EventId)],
    ) -> bool {
        self.commit_file_completion_with_ancestors(conversation, candidates, &[])
    }

    fn commit_file_completion_with_ancestors(
        &self,
        conversation: ConversationId,
        candidates: &[(FileCard, EventId)],
        ancestors: &[(EventId, FileCard, EventId)],
    ) -> bool {
        let mut store = self.delivery.lock().expect("delivery lock not poisoned");
        if self.recover_delivery(&mut store).is_err() {
            return false;
        }
        let mut committed = false;
        for (card, final_chunk) in candidates {
            if store.file_card(card.id) != Some(card) {
                continue;
            }
            // Cache aliases depend on an exact retained completion. Recheck
            // that dependency under the same lock as the new durable record.
            let alias_retained = ancestors.is_empty() || {
                let rows = self.received_files.lock().expect("files lock not poisoned");
                rows.entry(card.id).is_some_and(|row| {
                    let deadline = crate::expiry::deadline(&row.plaintext);
                    deadline.is_none_or(|time| time > super::node::now_millis())
                        && ancestors.iter().any(|(alias, prior, _)| {
                            *alias == card.id
                                && rows.entry(prior.id).is_some_and(|prior_row| {
                                    crate::expiry::deadline(&prior_row.plaintext) == deadline
                                })
                        })
                })
            };
            if !ancestors.is_empty()
                && (!alias_retained
                    || store.manifest_event_erased(card.id)
                    || self
                        .delivery_suspended
                        .load(std::sync::atomic::Ordering::Acquire)
                    || !ancestors.iter().any(|(alias, prior, prior_chunk)| {
                        *alias == card.id
                            && prior.conversation == card.conversation
                            && prior.completion_binding == card.completion_binding
                            && prior_chunk == final_chunk
                            && store.file_card(prior.id) == Some(prior)
                            && store.file_completion(prior.id) == Some(*prior_chunk)
                            && !store.manifest_event_erased(prior.id)
                    }))
            {
                continue;
            }
            if store.complete_file(card.id, *final_chunk).is_err() {
                return false;
            }
            committed = true;
        }
        // Source fanout still needs the original signed chunks after a local export.
        if store
            .cards_for_file(conversation)
            .iter()
            .any(|card| card.destinations.iter().any(|d| d.active))
        {
            return false;
        }
        if self.account_group_file_preparation_pinned(conversation) {
            return false;
        }
        if !committed && !self.untracked_file_reclaimable(conversation, &store) {
            return false;
        }
        let own_source = self
            .retained_file_rows(conversation)
            .iter()
            .any(|row| row.from == self.user_id());
        if self.is_channel_file_replica(conversation)
            && (own_source || self.shares_replica(&conversation))
        {
            self.prune_channel_file_replicas();
            return true;
        }
        let _ = self
            .log
            .lock()
            .expect("log lock not poisoned")
            .drop_conversation(&conversation);
        true
    }

    fn is_channel_file_replica(&self, conversation: ConversationId) -> bool {
        let media = {
            let files = self.files.lock().expect("files lock not poisoned");
            let Some(manifest) = files.manifest(&conversation) else {
                return false;
            };
            match manifest.kind() {
                Some(crate::file::FileKind::File) => false,
                _ if super::media_store::manifest_is_media(manifest) => true,
                _ => return false,
            }
        };
        self.retained_file_rows(conversation).iter().any(|row| {
            self.channels
                .lock()
                .expect("channels lock not poisoned")
                .state(&row.conversation)
                .is_some_and(|channel| {
                    // A local media preview is not recipient delivery. Its
                    // original signed chunks must survive incoming fingerprint
                    // probes and remain available to other channel members.
                    !media
                        || channel
                            .members()
                            .iter()
                            .any(|member| member.user_id() != self.user_id())
                })
        })
    }

    /// Keep complete channel files newest-first within a fixed local storage budget.
    /// Chunk conversations are independent DAGs, so pruning a whole conversation
    /// cannot orphan a message or another chunk. A peer may later download the file
    /// again from another source if its local replica has been evicted.
    fn prune_channel_file_replicas(&self) {
        let file_convs = self
            .files
            .lock()
            .expect("files lock not poisoned")
            .file_convs();
        let mut replicas = Vec::new();
        for file in file_convs {
            if self.account_group_file_preparation_pinned(file)
                || !self.is_channel_file_replica(file)
            {
                continue;
            }
            let rows = self.retained_file_rows(file);
            if rows.is_empty() {
                continue;
            }
            let Some(total) = self
                .files
                .lock()
                .expect("files lock not poisoned")
                .manifest(&file)
                .map(|manifest| manifest.chunk_count() as usize)
            else {
                continue;
            };
            let log = self.log.lock().expect("log lock not poisoned");
            let chunks = log.events(&file);
            if chunks.len() != total {
                continue;
            }
            let bytes = chunks
                .iter()
                .map(|event| event.ciphertext.len() + 256)
                .sum::<usize>();
            let newest = rows.iter().map(|row| row.wall_clock).max().unwrap_or(0);
            replicas.push((file, newest, bytes));
        }
        let drop = channel_file_evictions(replicas, CHANNEL_FILE_REPLICA_BUDGET);
        let mut log = self.log.lock().expect("log lock not poisoned");
        for file in drop {
            let _ = log.drop_conversation(&file);
        }
    }

    /// Own completed fanout and untracked group files have no recipient probe debt.
    /// Imported DM rows without a reservation remain conservatively retained.
    fn untracked_file_reclaimable(
        &self,
        conversation: ConversationId,
        store: &super::delivery_store::DeliveryStore,
    ) -> bool {
        let rows: Vec<_> = self
            .retained_file_rows(conversation)
            .into_iter()
            .filter(|row| {
                !store.manifest_event_erased(row.event_id)
                    && self
                        .channels
                        .lock()
                        .expect("channels lock not poisoned")
                        .state(&row.conversation)
                        .is_some()
            })
            .map(|row| (row.event_id, row.conversation))
            .collect();
        let log = self.log.lock().expect("log lock not poisoned");
        let chunks = log.events(&conversation);
        let Some(first) = chunks.first() else {
            return false;
        };
        let source = first.author;
        let allowed = source.ed25519_pub() == &self.identity.public().ed25519_pub
            || rows.iter().any(|(id, parent)| {
                log.get(id).is_some_and(|event| {
                    event.conversation_id == *parent
                        && event.kind == EventKind::FileManifest
                        && event.author == source
                        && event.verify_integrity()
                        && event.verify_signature()
                })
            });
        allowed
            && chunks.iter().enumerate().all(|(index, event)| {
                event.kind == EventKind::Message
                    && event.author == source
                    && event.seq == index as u64 + 1
                    && event.parents
                        == if index == 0 {
                            vec![]
                        } else {
                            vec![chunks[index - 1].id]
                        }
                    && event.verify_integrity()
                    && event.verify_signature()
            })
    }

    fn retained_file_rows(
        &self,
        conversation: ConversationId,
    ) -> Vec<super::received_log::ReceivedEntry> {
        let (manifest, ids) = {
            let files = self.files.lock().expect("files lock not poisoned");
            let Some(manifest) = files.manifest(&conversation).cloned() else {
                return vec![];
            };
            (manifest, files.manifest_events(conversation))
        };
        let records = self.received_files.lock().expect("files lock not poisoned");
        ids.into_iter()
            .filter_map(|id| records.entry(id))
            .filter(|row| {
                super::files::validated_manifest(&row.plaintext)
                    .is_some_and(|stored| super::files::same_file_transfer(&manifest, &stored))
            })
            .cloned()
            .collect()
    }

    /// A local export backing path. Group/own media eligibility never enters network proofs.
    pub(in crate::node) fn managed_media_for_export(
        &self,
        conversation: ConversationId,
        manifest: &crate::file::AnyManifest,
    ) -> Option<PathBuf> {
        let path = self.media.path_for(conversation, manifest.name());
        let metadata = path.metadata().ok()?;
        if !metadata.is_file() {
            return None;
        }
        if self.has_verified_file_completion(conversation) {
            return Some(path);
        }
        for row in self.retained_file_rows(conversation) {
            if self
                .delivery
                .lock()
                .expect("delivery lock not poisoned")
                .manifest_event_erased(row.event_id)
            {
                continue;
            }
            let event = self
                .log
                .lock()
                .expect("log lock not poisoned")
                .get(&row.event_id)
                .cloned()
                .or_else(|| {
                    self.account_file_proofs
                        .lock()
                        .expect("file proofs lock not poisoned")
                        .get(&row.event_id)
                        .cloned()
                });
            let Some(event) = event else {
                continue;
            };
            let own = event.author.ed25519_pub() == &self.identity.public().ed25519_pub;
            let channel = event.conversation_id == row.conversation
                && self
                    .channels
                    .lock()
                    .expect("channels lock not poisoned")
                    .state(&row.conversation)
                    .is_some();
            let sibling_proof = self
                .account_file_proofs
                .lock()
                .expect("file proofs lock not poisoned")
                .get(&row.event_id)
                .is_some();
            if (own || channel || sibling_proof)
                && event.kind == EventKind::FileManifest
                && event.author.user_id() == row.from
                && event.verify_integrity()
                && event.verify_signature()
            {
                return Some(path);
            }
        }
        None
    }

    pub(in crate::node) fn has_verified_file_completion(
        &self,
        conversation: ConversationId,
    ) -> bool {
        let store = self.delivery.lock().expect("delivery lock not poisoned");
        store.cards_for_file(conversation).iter().any(|card| {
            store.file_completed(card.id)
                && card
                    .completion_binding
                    .as_ref()
                    .is_some_and(|b| b.owner_account == self.account_id())
        })
    }

    pub(in crate::node) fn historical_file_completion(
        &self,
        conversation: ConversationId,
        final_chunk: EventId,
        peer: &PublicIdentity,
    ) -> bool {
        if self
            .delivery_suspended
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return false;
        }
        // Drop delivery before policy/proof lookup: erase/recovery acquire delivery first.
        let cards = self
            .delivery
            .lock()
            .expect("delivery lock not poisoned")
            .completed_files(conversation, final_chunk);
        cards.iter().any(|card| {
            let Some(binding) = &card.completion_binding else {
                return false;
            };
            let group = self.group_file_offer(conversation, card.id, peer).is_some();
            if binding.source != *peer
                || binding.owner_account != self.account_id()
                || (!self.known_account_allowed(peer) && !group)
            {
                return false;
            }
            let Some(proof) = self.historical_author(&peer.ed25519_pub) else {
                return false;
            };
            if proof.public() != *peer || proof.account_cert != binding.certificate {
                return false;
            }
            let original = self
                .log
                .lock()
                .expect("log lock not poisoned")
                .get(&card.id)
                .cloned();
            let row = self
                .received_files
                .lock()
                .expect("files lock not poisoned")
                .entry(card.id)
                .cloned();
            let retained = match (original, row) {
                (Some(event), Some(row)) => {
                    event.kind == EventKind::FileManifest
                        && (group
                            || event.conversation_id
                                == super::conversation::dm_conversation_id(
                                    &self.identity.public(),
                                    peer,
                                ))
                        && event.author.ed25519_pub() == &peer.ed25519_pub
                        && event.verify_integrity()
                        && event.verify_signature()
                        && row.conversation == card.conversation
                        && super::files::validated_manifest(&row.plaintext).is_some_and(
                            |manifest| {
                                manifest.file_conv() == conversation
                                    && manifest.chunk_count() == card.chunk_count
                            },
                        )
                }
                _ => false,
            };
            if !retained {
                return false;
            }
            let store = self.delivery.lock().expect("delivery lock not poisoned");
            store.file_card(card.id) == Some(card)
                && store
                    .completed_files(conversation, final_chunk)
                    .iter()
                    .any(|live| live.id == card.id)
                && !store.manifest_event_erased(card.id)
                && binding.owner_account == self.account_id()
                && !self
                    .delivery_suspended
                    .load(std::sync::atomic::Ordering::Acquire)
        })
    }
}

#[cfg(test)]
mod replica_tests {
    use super::*;

    #[test]
    fn erased_ancestor_cannot_commit_a_prepared_alias_completion() {
        use crate::discovery::Announce;
        use crate::eventlog::Event;
        use crate::identity::{account::Account, device::DeviceIdentity};
        let dir = tempfile::tempdir().unwrap();
        let source = DeviceIdentity::generate();
        let source_account = Account::generate();
        let proof = Announce::new_with_account(&source, &source_account, "source", 4000);
        let (node, _, _) = super::super::test_support::node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let host = super::super::conversation::dm_conversation_id(
            &source.public(),
            &node.identity.public(),
        );
        let file = ConversationId::new([73; 32]);
        let manifest = crate::file::FileManifest {
            name: "empty.txt".into(),
            size: 0,
            mime: "text/plain".into(),
            checksum: crate::file::file_checksum(&[]),
            file_key: [3; 32],
            file_conv: file,
            chunk_count: 1,
        };
        let mut cards = Vec::new();
        for sequence in 1..=2 {
            let event = Event::new(
                &source,
                host,
                sequence,
                vec![],
                sequence,
                sequence,
                EventKind::FileManifest,
                crate::dm::seal(
                    &source,
                    &node.identity.public().x25519_pub,
                    &manifest.encode(),
                )
                .unwrap(),
            );
            node.accept_manifest_event(
                &event,
                &proof,
                super::super::received_log::ReceivedEntry {
                    event_id: event.id,
                    conversation: host,
                    from: source.user_id(),
                    wall_clock: sequence,
                    plaintext: manifest.encode(),
                },
            )
            .unwrap();
            cards.push(
                node.delivery
                    .lock()
                    .unwrap()
                    .file_card(event.id)
                    .unwrap()
                    .clone(),
            );
        }
        let final_chunk = EventId::new([74; 32]);
        node.delivery
            .lock()
            .unwrap()
            .complete_file(cards[0].id, final_chunk)
            .unwrap();
        let prepared = vec![(cards[1].id, cards[0].clone(), final_chunk)];
        let target_row = node
            .received_files
            .lock()
            .unwrap()
            .entry(cards[1].id)
            .unwrap()
            .clone();
        for deadline in [
            super::super::node::now_millis() - 1,
            super::super::node::now_millis() + 60_000,
        ] {
            node.received_files
                .lock()
                .unwrap()
                .remove_where(|row| row.event_id == cards[1].id)
                .unwrap();
            let mut changed = target_row.clone();
            changed.plaintext = crate::expiry::wrap(&changed.plaintext, Some(deadline));
            node.received_files
                .lock()
                .unwrap()
                .record_durable(&changed)
                .unwrap();
            assert!(!node.commit_file_completion_with_ancestors(
                file,
                &[(cards[1].clone(), final_chunk)],
                &prepared
            ));
            assert!(!node.delivery.lock().unwrap().file_completed(cards[1].id));
        }
        node.received_files
            .lock()
            .unwrap()
            .remove_where(|row| row.event_id == cards[1].id)
            .unwrap();
        node.received_files
            .lock()
            .unwrap()
            .record_durable(&target_row)
            .unwrap();
        node.delivery
            .lock()
            .unwrap()
            .erase_file(host, cards[0].id)
            .unwrap();
        assert!(!node.commit_file_completion_with_ancestors(
            file,
            &[(cards[1].clone(), final_chunk)],
            &prepared
        ));
        assert!(!node.delivery.lock().unwrap().file_completed(cards[1].id));
    }

    #[test]
    fn channel_file_budget_keeps_recent_complete_files_and_evicts_older_ones() {
        let a = ConversationId::new([1; 32]);
        let b = ConversationId::new([2; 32]);
        let c = ConversationId::new([3; 32]);
        assert_eq!(
            channel_file_evictions(vec![(a, 1, 5), (b, 2, 5), (c, 3, 20)], 9),
            vec![c, a]
        );
        assert_eq!(
            channel_file_evictions(vec![(a, 1, 5), (b, 2, 5), (c, 3, 5)], 10),
            vec![a]
        );
    }
}
