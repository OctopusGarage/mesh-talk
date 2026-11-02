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
    #[cfg(test)]
    pub(in crate::node) fn operation_input_consumed_for_test(
        &self,
        meta: &AccountDocumentMeta,
    ) -> bool {
        Self::operation_input_key(&meta.key).is_some_and(|key| {
            self.operation_seen.lock().unwrap().get(&key) == Some(&meta.content_hash)
        })
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
        let mut projection = match self.operation_projection(group) {
            Ok(projection) => projection,
            Err(error) => {
                log::debug!("group operation preflight deferred: {error}");
                return false;
            }
        };
        projection.requests.push(request);
        projection.requests.len() <= 8 && self.operation_projection_fits(group, projection)
    }
    pub(in crate::node) fn preflight_group_operation_lookup(
        &self,
        lookup: LookupProjection,
    ) -> bool {
        let group = lookup.group;
        let mut projection = match self.operation_projection(group) {
            Ok(projection) => projection,
            Err(error) => {
                log::debug!("group operation preflight deferred: {error}");
                return false;
            }
        };
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
            .operation_publication_groups(false)
            .into_iter()
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
            #[cfg(test)]
            let phase_clock = std::time::Instant::now();
            #[cfg(test)]
            let owner_phase = |phase: &str| {
                self.trace_account_client(&self.identity.public(), || {
                    format!(
                        "operation_publication_owner group={} phase={phase} elapsed_ms={}",
                        hex::encode(group.as_bytes()),
                        phase_clock.elapsed().as_millis()
                    )
                });
            };
            #[cfg(test)]
            owner_phase("projection_begin");
            let projection = match self.operation_projection(group) {
                Ok(projection) => projection,
                Err(error) => {
                    log::debug!("group operation publication deferred: {error}");
                    continue;
                }
            };
            let key = AccountDocumentKey::GroupOperation {
                channel: hex::encode(group.as_bytes()),
                source_device: self.user_id(),
            };
            let verified = self
                .operation_seen
                .lock()
                .expect("operation input mutex")
                .get(&Self::operation_input_key(&key).expect("operation key"))
                .copied();
            #[cfg(test)]
            let published_revision;
            let changed = {
                #[cfg(test)]
                owner_phase("projection_end_store_lock_begin");
                let mut store = self.account_documents.lock().expect("documents mutex");
                #[cfg(test)]
                owner_phase("store_lock_acquired_read_begin");
                let previous = store.meta(&key);
                let old = store.get(&key).ok().flatten().and_then(|document| {
                    // The input owner already verified these exact bytes. Reuse
                    // that fact only for this device's own publication; changed,
                    // unreadable or foreign frames retain full verification.
                    if verified == Some(document.meta.content_hash)
                        && let Ok(frame) =
                            serde_json::from_slice::<OperationFrame>(document.payload())
                        && frame.proof.public() == self.identity.public()
                        && frame.group == group
                        && frame.revision == document.meta.revision
                    {
                        return Some(frame);
                    }
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
                #[cfg(test)]
                owner_phase("old_read_end_frame_begin");
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
                #[cfg(test)]
                owner_phase("frame_end_merge_begin");
                let changed = store.merge(meta, &payload);
                #[cfg(test)]
                owner_phase("merge_end");
                changed
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

    #[tokio::test]
    async fn unchanged_local_operation_publication_does_not_repeat_signature_verification() {
        let directory = tempfile::tempdir().unwrap();
        let account = crate::identity::account::Account::generate();
        let source = super::super::tests::node(&directory.path().join("source"), &account);
        let group = source
            .create_channel("Before publication", vec![])
            .await
            .unwrap();
        let id = source
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        source
            .submit_account_group_operation(
                &account.account_id(),
                id,
                group,
                crate::node::AccountGroupOperation::Rename {
                    name: "Committed publication".into(),
                },
            )
            .unwrap();
        source.maintain_account_group_operation_effects();
        assert_eq!(
            source
                .account_group_operation_status(&account.account_id(), id)
                .unwrap()
                .unwrap()
                .phase,
            crate::node::AccountGroupOperationPhase::Committed
        );
        let legacy_key = AccountDocumentKey::GroupLegacyOperation {
            channel: hex::encode(group.as_bytes()),
            source_device: source.user_id(),
        };
        let legacy =
            LegacyOperationFrame::new(&source, group, 1, LegacyOperationProjection::default())
                .encode();
        source
            .account_documents
            .lock()
            .unwrap()
            .merge(
                AccountDocumentMeta::new(
                    &account,
                    source.user_id(),
                    legacy_key.clone(),
                    1,
                    &legacy,
                    false,
                ),
                &legacy,
            )
            .unwrap();
        // Process the real local frame through the existing verified-input owner.
        source.maintain_account_group_operation_effects();
        let key = AccountDocumentKey::GroupOperation {
            channel: hex::encode(group.as_bytes()),
            source_device: source.user_id(),
        };
        let before = source.account_documents.lock().unwrap().meta(&key).unwrap();
        operations::take_frame_decode_counts_for_test();
        source.publish_account_group_operations();
        source.publish_account_group_legacy_operations();
        let unchanged = operations::take_frame_decode_counts_for_test();
        assert_eq!(
            source.account_documents.lock().unwrap().meta(&key).unwrap(),
            before
        );
        assert_eq!(
            unchanged,
            [0, 0],
            "unchanged locally verified publication must not repeat signed-reference verification under the document lock"
        );
        // A new process has no verified-input witness: it must authenticate the
        // durable rows again rather than trusting a stored local-looking key.
        source.operation_seen.lock().unwrap().clear();
        source.publish_account_group_operations();
        source.publish_account_group_legacy_operations();
        assert_eq!(operations::take_frame_decode_counts_for_test(), [1, 1]);
        // A signed replacement has a new hash and cannot borrow the old witness.
        source.maintain_account_group_operation_effects();
        let projection = source.operation_projection(group).unwrap();
        let replacement =
            OperationFrame::new(&source, group, before.revision + 1, projection).encode();
        source
            .account_documents
            .lock()
            .unwrap()
            .merge(
                AccountDocumentMeta::new(
                    &account,
                    source.user_id(),
                    key.clone(),
                    before.revision + 1,
                    &replacement,
                    false,
                ),
                &replacement,
            )
            .unwrap();
        operations::take_frame_decode_counts_for_test();
        source.publish_account_group_operations();
        assert_eq!(operations::take_frame_decode_counts_for_test(), [1, 0]);
        source.maintain_account_group_operation_effects();
        operations::take_frame_decode_counts_for_test();
        source.publish_account_group_operations();
        source.publish_account_group_legacy_operations();
        assert_eq!(operations::take_frame_decode_counts_for_test(), [0, 0]);
    }

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
