//! Local verified completion survives reclamation, without extending the wire protocol.
use super::{delivery_store::FileCard, Node};
use crate::{
    eventlog::{ConversationId, EventId, EventKind},
    identity::device::PublicIdentity,
};
use std::path::PathBuf;

impl Node {
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
        let mut store = self.delivery.lock().expect("delivery lock not poisoned");
        if self.recover_delivery(&mut store).is_err() {
            return false;
        }
        let mut committed = false;
        for (card, final_chunk) in candidates {
            if store.file_card(card.id) != Some(card) {
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
        if !committed && !self.untracked_file_reclaimable(conversation, &store) {
            return false;
        }
        let _ = self
            .log
            .lock()
            .expect("log lock not poisoned")
            .drop_conversation(&conversation);
        true
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
                .cloned();
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
            if (own || channel)
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
            if binding.source != *peer
                || binding.owner_account != self.account_id()
                || !self.known_account_allowed(peer)
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
                        && event.conversation_id
                            == super::conversation::dm_conversation_id(
                                &self.identity.public(),
                                peer,
                            )
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
