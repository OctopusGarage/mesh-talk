//! Mandatory bounded operation projections reuse the existing account data channel.
use super::*;
use crate::node::account_group::operations::wire::{
    LookupProjection, MAX_HEAD, OperationFrame, OperationProjection, RequestProjection,
};

impl Node {
    pub(super) fn operation_input_key(key: &AccountDocumentKey) -> Option<String> {
        match key {
            AccountDocumentKey::GroupOperation {
                channel,
                source_device,
            } => Some(format!("{channel}:{source_device}")),
            AccountDocumentKey::GroupLegacyOperation {
                channel,
                source_device,
            } => Some(format!("legacy:{channel}:{source_device}")),
            _ => None,
        }
    }
    // Only maintenance consults the existing processed-input owner. Capture
    // hashes for CURRENT row keys without nesting seen/store locks or copying
    // unrelated legacy-parent query tokens. Recheck current metadata under the
    // payload guard so a changed head can never be skipped using an old hash.
    pub(super) fn operation_input_documents(
        &self,
        legacy: bool,
        maintenance: bool,
    ) -> Vec<(AccountDocumentMeta, AccountDocument)> {
        let rows = {
            let store = self.account_documents.lock().expect("documents mutex");
            if legacy {
                store.legacy_operation_rows()
            } else {
                store.operation_rows()
            }
        };
        let selected: Vec<_> = if maintenance {
            let seen = self.operation_seen.lock().expect("operation input mutex");
            rows.into_iter()
                .map(|meta| {
                    let hash = Self::operation_input_key(&meta.key)
                        .and_then(|key| seen.get(&key).copied());
                    (meta.key, hash)
                })
                .collect()
        } else {
            rows.into_iter().map(|meta| (meta.key, None)).collect()
        };
        let store = self.account_documents.lock().expect("documents mutex");
        selected
            .into_iter()
            .filter_map(|(key, seen)| {
                let meta = store.meta(&key)?;
                if maintenance && seen == Some(meta.content_hash) {
                    return None;
                }
                store
                    .get(&key)
                    .ok()
                    .flatten()
                    .map(|document| (meta, document))
            })
            .collect()
    }
    #[cfg(test)]
    pub(in crate::node) fn account_group_operation_inputs(
        &self,
    ) -> Vec<(String, [u8; 32], OperationFrame)> {
        self.operation_inputs(false)
    }
    pub(in crate::node) fn unseen_account_group_operation_inputs(
        &self,
    ) -> Vec<(String, [u8; 32], OperationFrame)> {
        self.operation_inputs(true)
    }
    fn operation_inputs(&self, maintenance: bool) -> Vec<(String, [u8; 32], OperationFrame)> {
        let documents = self.operation_input_documents(false, maintenance);
        documents
            .into_iter()
            .filter_map(|(meta, document)| {
                let frame =
                    OperationFrame::decode(document.payload(), &self.account.public().ed25519_pub)?;
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

#[cfg(test)]
mod input_tests {
    use super::*;
    use crate::node::account_group::operations::{
        self,
        legacy_wire::{LegacyOperationFrame, LegacyOperationProjection},
    };

    fn put_head(reader: &Node, author: &Node, value: u8, revision: u64, legacy: bool) {
        let group = crate::eventlog::ConversationId::new([value; 32]);
        let (key, payload) = if legacy {
            (
                AccountDocumentKey::GroupLegacyOperation {
                    channel: hex::encode(group.as_bytes()),
                    source_device: author.user_id(),
                },
                LegacyOperationFrame::new(
                    author,
                    group,
                    revision,
                    LegacyOperationProjection::default(),
                )
                .encode(),
            )
        } else {
            (
                AccountDocumentKey::GroupOperation {
                    channel: hex::encode(group.as_bytes()),
                    source_device: author.user_id(),
                },
                OperationFrame::new(author, group, revision, OperationProjection::default())
                    .encode(),
            )
        };
        let meta = AccountDocumentMeta::new(
            &author.account,
            author.user_id(),
            key,
            revision,
            &payload,
            false,
        );
        reader
            .account_documents
            .lock()
            .unwrap()
            .merge(meta, &payload)
            .unwrap();
    }

    #[tokio::test]
    async fn unchanged_processed_operation_heads_skip_repeated_signature_decodes() {
        let directory = tempfile::tempdir().unwrap();
        let account = crate::identity::account::Account::generate();
        let reader = super::super::tests::node(&directory.path().join("reader"), &account);
        let foreign = super::super::tests::node(&directory.path().join("foreign"), &account);
        for value in 1..=33 {
            let author = if value % 2 == 0 { &reader } else { &foreign };
            put_head(&reader, author, value, 1, false);
            put_head(&reader, author, value, 1, true);
        }
        operations::take_frame_decode_counts_for_test();
        reader.maintain_account_group_operation_effects();
        let first = operations::take_frame_decode_counts_for_test();
        assert_eq!(
            first,
            [33, 33],
            "first maintenance must verify all66 actual signed normal/legacy frames"
        );
        reader.maintain_account_group_operation_effects();
        assert_eq!(
            operations::take_frame_decode_counts_for_test(),
            [0, 0],
            "unchanged processed or fully verified own heads cannot repeat signature decoding before the existing seen check"
        );
        reader
            .set_device_sync_category(crate::node::SyncCategory::History, false)
            .unwrap();
        reader.maintain_account_group_operation_effects();
        assert_eq!(
            operations::take_frame_decode_counts_for_test(),
            [0, 0],
            "policy changes do not re-authorize already processed inputs"
        );
        put_head(&reader, &foreign, 1, 2, false);
        put_head(&reader, &foreign, 1, 2, true);
        operations::take_frame_decode_counts_for_test();
        reader.maintain_account_group_operation_effects();
        assert_eq!(
            operations::take_frame_decode_counts_for_test(),
            [1, 1],
            "changed foreign heads still decode and enter ordinary processing"
        );
        reader.maintain_account_group_operation_effects();
        assert_eq!(operations::take_frame_decode_counts_for_test(), [0, 0]);
        assert_eq!(
            reader.account_group_operation_inputs().len(),
            17,
            "generic input projection remains a complete diagnostic view"
        );
        assert_eq!(reader.account_group_legacy_inputs().len(), 17);
        operations::take_frame_decode_counts_for_test();
        reader
            .configure_privacy(
                &directory.path().join("reader"),
                "pw",
                &reader.signed_announce("decode guard", 1),
                std::sync::Arc::new(crate::discovery::DiscoveryVisibility::default()),
            )
            .unwrap();
        let other_account = crate::identity::account::Account::generate();
        let rebound = crate::discovery::Announce::new_with_account(
            &foreign.identity,
            &other_account,
            "rebound",
            1,
        );
        reader
            .privacy
            .state
            .write()
            .unwrap()
            .as_mut()
            .unwrap()
            .proofs
            .record_explicit_account(&rebound)
            .unwrap();
        put_head(&reader, &foreign, 77, 1, false);
        put_head(&reader, &foreign, 77, 1, true);
        operations::take_frame_decode_counts_for_test();
        for _ in 0..2 {
            reader.maintain_account_group_operation_effects();
            assert_eq!(
                operations::take_frame_decode_counts_for_test(),
                [1, 1],
                "valid signed but currently unbound foreign heads remain retryable, never seen"
            );
        }
        let normal_key = format!("{}:{}", hex::encode([77; 32]), foreign.user_id());
        assert!(
            !reader
                .operation_seen
                .lock()
                .unwrap()
                .contains_key(&normal_key)
        );
        assert!(
            !reader
                .operation_seen
                .lock()
                .unwrap()
                .contains_key(&format!("legacy:{normal_key}"))
        );
        reader
            .privacy
            .state
            .write()
            .unwrap()
            .as_mut()
            .unwrap()
            .proofs
            .record_explicit_account(&foreign.own_group_proof())
            .unwrap();
        reader.maintain_account_group_operation_effects();
        assert_eq!(
            operations::take_frame_decode_counts_for_test(),
            [1, 1],
            "restored genuine current binding allows ordinary verification and processing"
        );
        reader.maintain_account_group_operation_effects();
        assert_eq!(operations::take_frame_decode_counts_for_test(), [0, 0]);
    }
}
