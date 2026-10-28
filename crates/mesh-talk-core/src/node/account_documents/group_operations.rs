//! Mandatory bounded operation projections reuse the existing account data channel.
use super::*;
use crate::node::account_group::operations::wire::{
    LookupProjection, MAX_HEAD, OperationFrame, OperationProjection, RequestProjection,
};

impl Node {
    pub(in crate::node) fn account_group_operation_inputs(
        &self,
    ) -> Vec<(String, [u8; 32], OperationFrame)> {
        let documents: Vec<_> = {
            let store = self.account_documents.lock().expect("documents mutex");
            store
                .operation_rows()
                .into_iter()
                .filter_map(|meta| {
                    store
                        .get(&meta.key)
                        .ok()
                        .flatten()
                        .map(|document| (meta, document))
                })
                .collect()
        };
        documents
            .into_iter()
            .filter_map(|(meta, document)| {
                let frame =
                    OperationFrame::decode(document.payload(), &self.account.public().ed25519_pub)?;
                if frame.proof.public() == self.identity.public()
                    || !self.account_group_binding_current(&frame.proof)
                {
                    return None;
                }
                Some((
                    format!(
                        "{}:{}",
                        hex::encode(frame.group.as_bytes()),
                        frame.proof.public().user_id()
                    ),
                    meta.content_hash,
                    frame,
                ))
            })
            .collect()
    }
    pub(in crate::node) fn preflight_group_operation_request(
        &self,
        request: RequestProjection,
    ) -> bool {
        let group = request.reference.group;
        let mut projection = self.operation_projection(group);
        projection.requests.push(request);
        projection.requests.len() <= 8 && self.operation_projection_fits(group, projection)
    }
    pub(in crate::node) fn preflight_group_operation_lookup(
        &self,
        lookup: LookupProjection,
    ) -> bool {
        let group = lookup.group;
        let mut projection = self.operation_projection(group);
        projection.lookups.push(lookup);
        projection.lookups.len() <= 8 && self.operation_projection_fits(group, projection)
    }
    fn operation_projection_fits(
        &self,
        group: crate::eventlog::ConversationId,
        projection: OperationProjection,
    ) -> bool {
        let key = AccountDocumentKey::GroupOperation {
            channel: hex::encode(group.as_bytes()),
            source_device: self.user_id(),
        };
        let bytes = OperationFrame::new(self, group, 1, projection)
            .encode()
            .len()
            .saturating_add(512);
        bytes <= MAX_HEAD
            && self
                .account_documents
                .lock()
                .expect("documents mutex")
                .operation_payload_fits(&key, bytes)
    }
    pub(in crate::node) fn publish_account_group_operations(&self) {
        let groups: Vec<_> = self
            .operation_groups()
            .into_iter()
            .take(128)
            .map(crate::eventlog::ConversationId::new)
            .collect();
        self.publish_account_group_operation_groups(&groups);
    }
    pub(in crate::node) fn publish_account_group_operations_for(
        &self,
        group: crate::eventlog::ConversationId,
    ) {
        self.publish_account_group_operation_groups(&[group]);
    }
    fn publish_account_group_operation_groups(&self, groups: &[crate::eventlog::ConversationId]) {
        for &group in groups {
            let projection = self.operation_projection(group);
            let key = AccountDocumentKey::GroupOperation {
                channel: hex::encode(group.as_bytes()),
                source_device: self.user_id(),
            };
            #[cfg(test)]
            let published_revision;
            let changed = {
                let mut store = self.account_documents.lock().expect("documents mutex");
                let previous = store.meta(&key);
                let old = store.get(&key).ok().flatten().and_then(|document| {
                    OperationFrame::decode(document.payload(), &self.account.public().ed25519_pub)
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
                let frame = OperationFrame::new(self, group, revision, projection);
                let payload = frame.encode();
                if payload.len() > MAX_HEAD || !store.operation_payload_fits(&key, payload.len()) {
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
                #[cfg(test)]
                {
                    published_revision = revision;
                }
                store.merge(meta, &payload)
            };
            #[cfg(test)]
            if matches!(&changed, Ok(true)) {
                self.trace_account_client(&self.identity.public(), || {
                    format!("operation_local_published revision={published_revision}")
                });
            }
            match changed {
                Ok(true) => self.documents_changed(),
                Ok(false) => {}
                Err(error) => log::debug!("operation projection awaits repair: {error}"),
            }
        }
    }
}
