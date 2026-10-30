//! Negotiated private legacy child rows never enter ordinary operation ticket projections.
use super::*;
use crate::node::account_group::operations::legacy_wire::{LegacyOperationFrame, MAX_LEGACY_HEAD};

impl Node {
    #[cfg(test)]
    pub(in crate::node) fn account_group_legacy_inputs(
        &self,
    ) -> Vec<(String, [u8; 32], LegacyOperationFrame)> {
        self.legacy_operation_inputs(false)
    }
    pub(in crate::node) fn unseen_account_group_legacy_inputs(
        &self,
    ) -> Vec<(String, [u8; 32], LegacyOperationFrame)> {
        self.legacy_operation_inputs(true)
    }
    fn legacy_operation_inputs(
        &self,
        maintenance: bool,
    ) -> Vec<(String, [u8; 32], LegacyOperationFrame)> {
        let documents = self.operation_input_documents(true, maintenance);
        documents
            .into_iter()
            .filter_map(|(meta, document)| {
                let frame = LegacyOperationFrame::decode(
                    document.payload(),
                    &self.account.public().ed25519_pub,
                )?;
                if frame.proof.public() == self.identity.public() {
                    if maintenance {
                        self.operation_seen
                            .lock()
                            .expect("operation input mutex")
                            .insert(Self::operation_input_key(&meta.key)?, meta.content_hash);
                    }
                    return None;
                }
                if !self.account_group_binding_current(&frame.proof) {
                    return None;
                }
                Some((
                    format!(
                        "legacy:{}:{}",
                        hex::encode(frame.group.as_bytes()),
                        frame.proof.public().user_id()
                    ),
                    meta.content_hash,
                    frame,
                ))
            })
            .collect()
    }
    pub(in crate::node) fn publish_account_group_legacy_operations(&self) {
        let groups: Vec<_> = self
            .operation_groups()
            .into_iter()
            .take(128)
            .map(crate::eventlog::ConversationId::new)
            .collect();
        self.publish_account_group_legacy_operation_groups(&groups);
    }
    fn publish_account_group_legacy_operation_groups(
        &self,
        groups: &[crate::eventlog::ConversationId],
    ) {
        for &group in groups {
            let projection = self.legacy_leave_projection(group);
            let key = AccountDocumentKey::GroupLegacyOperation {
                channel: hex::encode(group.as_bytes()),
                source_device: self.user_id(),
            };
            let verified = self
                .operation_seen
                .lock()
                .expect("operation input mutex")
                .get(&Self::operation_input_key(&key).expect("legacy operation key"))
                .copied();
            let changed = {
                let mut store = self.account_documents.lock().expect("documents mutex");
                let previous = store.meta(&key);
                if previous.is_none()
                    && projection.requests.is_empty()
                    && projection.results.is_empty()
                {
                    continue;
                }
                let old = store.get(&key).ok().flatten().and_then(|document| {
                    // Exact verified own bytes need no repeated signature work
                    // merely to compare the current publication fingerprint.
                    if verified == Some(document.meta.content_hash)
                        && let Ok(frame) =
                            serde_json::from_slice::<LegacyOperationFrame>(document.payload())
                        && frame.proof.public() == self.identity.public()
                        && frame.group == group
                        && frame.revision == document.meta.revision
                    {
                        return Some(frame);
                    }
                    LegacyOperationFrame::decode(
                        document.payload(),
                        &self.account.public().ed25519_pub,
                    )
                });
                if old
                    .as_ref()
                    .is_some_and(|old| old.projection.fingerprint() == projection.fingerprint())
                {
                    continue;
                }
                let Some(revision) = previous.map_or(Some(1), |meta| meta.revision.checked_add(1))
                else {
                    continue;
                };
                let frame = LegacyOperationFrame::new(self, group, revision, projection);
                let payload = frame.encode();
                if payload.len() > MAX_LEGACY_HEAD
                    || !store.operation_payload_fits(&key, payload.len())
                {
                    continue;
                }
                let meta = AccountDocumentMeta::new(
                    &self.account,
                    self.user_id(),
                    key,
                    revision,
                    &payload,
                    false,
                );
                store.merge(meta, &payload)
            };
            match changed {
                Ok(true) => self.documents_changed(),
                Ok(false) => {}
                Err(error) => log::debug!("operation projection awaits repair: {error}"),
            }
        }
    }
}
