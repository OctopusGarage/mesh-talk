//! Mandatory bounded operation projections reuse the existing account data channel.
use super::*;
use crate::node::account_group::operations::wire::{
    LookupProjection, MAX_HEAD, OperationFrame, OperationProjection, RequestProjection,
};

impl Node {
    pub(in crate::node) fn lock_group_operation_publication(
        &self,
    ) -> std::sync::MutexGuard<'_, ()> {
        use std::sync::TryLockError;
        // Poison cannot invalidate this unit ordering gate: it contains no
        // payload. Journal/store guards retain their own fatal poison policy.
        match self.group_operation_publication.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => {
                #[cfg(test)]
                self.trace_account_client(&self.identity.public(), || {
                    "operation_owner phase=publication_contended".into()
                });
                self.group_operation_publication
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
            }
        }
    }

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
            let _publication = self.lock_group_operation_publication();
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
            let key = AccountDocumentKey::GroupOperation {
                channel: hex::encode(group.as_bytes()),
                source_device: self.user_id(),
            };
            let input_key = Self::operation_input_key(&key).expect("operation key");
            let verified = || {
                self.operation_seen
                    .lock()
                    .expect("operation input mutex")
                    .get(&input_key)
                    .copied()
            };
            let observe_input = || match self.operation_publication_input(group) {
                Ok(input) => input,
                Err(error) => {
                    // This observation only qualifies a shortcut. Keep the
                    // existing live publication and its own error handling.
                    log::debug!("group operation input observation unavailable: {error}");
                    None
                }
            };
            let input = observe_input();
            if let Some(input) = input {
                let verified = verified();
                let mut store = self.account_documents.lock().expect("documents mutex");
                let previous = store.meta(&key);
                if store.operation_publication_input_unchanged(previous.as_ref(), input, verified) {
                    continue;
                }
            }
            let projection = match self.operation_projection(group) {
                Ok(projection) => projection,
                Err(error) => {
                    log::debug!("group operation publication deferred: {error}");
                    continue;
                }
            };
            #[cfg(test)]
            owner_phase("projection_end_store_lock_begin");
            // A concurrent input change must not be acknowledged by this turn.
            // The existing live publication can finish, but it earns no input
            // qualification until a later stable materialization observes it.
            let input = input.filter(|before| observe_input() == Some(*before));
            let verified = verified();
            #[cfg(test)]
            let published_revision;
            let changed = {
                let mut store = self.account_documents.lock().expect("documents mutex");
                #[cfg(test)]
                owner_phase("store_lock_acquired_read_begin");
                let previous = store.meta(&key);
                let fingerprint = projection.fingerprint();
                if store.operation_publication_unchanged(previous.as_ref(), fingerprint, verified) {
                    if let Some(meta) = &previous {
                        store.qualify_operation_publication(meta, input);
                    }
                    continue;
                }
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
                    .is_some_and(|old| old.projection.fingerprint() == fingerprint)
                {
                    if let Some(meta) = previous {
                        store.remember_operation_publication(meta.clone(), fingerprint);
                        store.qualify_operation_publication(&meta, input);
                    }
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
                let changed = store.merge(meta.clone(), &payload);
                if changed.is_ok() {
                    store.remember_operation_publication(meta.clone(), fingerprint);
                    store.qualify_operation_publication(&meta, input);
                }
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
    async fn publication_unit_poison_recovers_without_poisoning_durable_owners() {
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
                    name: "After recovered publication".into(),
                },
            )
            .unwrap();
        source
            .commit_account_group_control_without_publication_for_test(group, id)
            .unwrap();
        *source.account_client_test_hook.lock().unwrap() = Some(std::sync::Arc::new(|_, phase| {
            if phase.starts_with("operation_publication_owner ")
                && phase.contains("phase=projection_begin ")
            {
                // The unit ordering guard is held; journal/document/log/member
                // owners have not been entered by this publication.
                panic!("injected publication ordering panic");
            }
        }));
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            source.publish_account_group_operations_for(group)
        }));
        *source.account_client_test_hook.lock().unwrap() = None;
        assert!(interrupted.is_err());
        assert!(source.group_operation_publication.is_poisoned());
        assert!(!source.group_operations.is_poisoned());
        assert!(!source.account_documents.is_poisoned());
        // A poisoned try_lock error owns the acquired unit guard. Recover it
        // directly; trying lock() again here would wait on that same guard.
        source.publish_account_group_operations_for(group);
        source.publish_account_group_legacy_operations();
        let key = AccountDocumentKey::GroupOperation {
            channel: hex::encode(group.as_bytes()),
            source_device: source.user_id(),
        };
        let document = source.account_document(&key).unwrap().unwrap();
        let frame =
            OperationFrame::decode(document.payload(), &account.public().ed25519_pub).unwrap();
        let canonical = source
            .retained_account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap();
        assert_eq!(
            canonical.phase,
            crate::node::AccountGroupOperationPhase::Committed
        );
        assert!(
            frame
                .projection
                .results
                .iter()
                .any(|row| row.status == canonical)
        );
        assert_eq!(frame.proof.public(), source.identity.public());
        assert_eq!(frame.group, group);
        assert_eq!(frame.revision, document.meta.revision);
        let event = source
            .log
            .lock()
            .unwrap()
            .get(&canonical.committed_events[0])
            .unwrap()
            .unwrap()
            .clone();
        assert!(event.verify_integrity() && event.verify_signature());
        assert_eq!(
            event.author.ed25519_pub(),
            &source.identity.public().ed25519_pub
        );
    }

    #[tokio::test]
    async fn unchanged_committed_text_and_file_publication_does_not_reload_event_bodies() {
        use crate::node::{
            AccountGroupOperationKind, AccountGroupOperationPhase, AccountGroupTextInput,
        };

        let directory = tempfile::tempdir().unwrap();
        let account = crate::identity::account::Account::generate();
        let source = super::super::tests::node(&directory.path().join("source"), &account);
        let group = source
            .create_channel("Settled operation results", vec![])
            .await
            .unwrap();
        let text_id = source
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        source
            .submit_account_group_text_if(
                &account.account_id(),
                text_id,
                group,
                AccountGroupTextInput {
                    text: b"unchanged committed text".to_vec(),
                    reply_to: None,
                    expires_at_ms: None,
                    protection: None,
                },
                |commit| commit(),
            )
            .unwrap();
        source.maintain_account_group_operation_effects();

        let file_path = directory.path().join("unchanged.bin");
        std::fs::write(&file_path, b"unchanged committed file").unwrap();
        let file_id = source
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        source
            .submit_account_group_file_if(
                &account.account_id(),
                file_id,
                group,
                &file_path,
                crate::file::FileKind::File,
                None,
                None,
                |_| {},
                |commit| commit(),
            )
            .await
            .unwrap();
        source.maintain_account_group_operation_effects();
        // Consume the locally signed frame through its normal verified-input
        // owner, then warm publication without admitting any background producer.
        source.maintain_account_group_operation_effects();
        source.publish_account_group_operations_for(group);
        let projection = source.operation_projection(group).unwrap();
        for (id, kind) in [
            (text_id, AccountGroupOperationKind::Text),
            (file_id, AccountGroupOperationKind::File),
        ] {
            let result = projection
                .results
                .iter()
                .find(|result| result.reference.id == id)
                .expect("the real committed result must be present in the publication");
            assert_eq!(result.reference.kind, kind);
            assert_eq!(result.status.phase, AccountGroupOperationPhase::Committed);
            let event = result.message_event.as_ref().unwrap();
            assert!(event.verify_integrity() && event.verify_signature());
            if kind == AccountGroupOperationKind::File {
                assert!(result.file_descriptor.is_some());
            }
        }
        let file_event = projection
            .results
            .iter()
            .find(|result| result.reference.id == file_id)
            .unwrap()
            .message_event
            .as_ref()
            .unwrap()
            .id;
        assert!(
            crate::expiry::deadline(
                &source
                    .received_files
                    .lock()
                    .unwrap()
                    .entry_in(&group, file_event)
                    .unwrap()
                    .unwrap()
                    .plaintext,
            )
            .is_none(),
            "the measured result has no time-triggered expiry"
        );

        let key = AccountDocumentKey::GroupOperation {
            channel: hex::encode(group.as_bytes()),
            source_device: source.user_id(),
        };
        let published = source.account_document(&key).unwrap().unwrap();
        assert!(
            OperationFrame::decode(published.payload(), &account.public().ed25519_pub).is_some()
        );
        let authority = || {
            let projection = source.group_projection_revision().unwrap();
            let event_source = {
                let log = source.log.lock().unwrap();
                let repository = log.repository().unwrap();
                (
                    repository.source_revision().unwrap(),
                    repository.destructive_policy_revision().unwrap(),
                )
            };
            let file_rows = {
                let rows = source.received_files.lock().unwrap();
                (rows.revision(), rows.rewrite_revision())
            };
            let recalls = source.recalls.lock().unwrap().revision();
            let suppression = source.account_suppression.lock().unwrap().len().unwrap();
            let operations = [text_id, file_id].map(|id| {
                (
                    source.group_operation_journal_revision_for_test(id),
                    source.operation_query_nonce(id),
                )
            });
            (
                projection,
                event_source,
                file_rows,
                recalls,
                suppression,
                operations,
            )
        };
        let before_authority = authority();
        let before_meta = source.account_documents.lock().unwrap().meta(&key).unwrap();
        let before_bodies = source
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        for _ in 0..16 {
            source.publish_account_group_operations_for(group);
        }
        let after_bodies = source
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        assert_eq!(
            source.account_documents.lock().unwrap().meta(&key).unwrap(),
            before_meta
        );
        assert_eq!(
            authority(),
            before_authority,
            "no source or authority input changed during publication"
        );
        assert_eq!(
            after_bodies, before_bodies,
            "unchanged committed Text/File publication must decide from current inputs before fetching and decoding event bodies"
        );
        let settled_input = source.operation_publication_input(group).unwrap().unwrap();
        let next = source
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        source
            .submit_account_group_text_if(
                &account.account_id(),
                next,
                group,
                AccountGroupTextInput {
                    text: b"new real input".to_vec(),
                    reply_to: None,
                    expires_at_ms: None,
                    protection: None,
                },
                |commit| commit(),
            )
            .unwrap();
        assert_eq!(
            source.operation_publication_input(group).unwrap(),
            None,
            "pending nonce/query/capability decisions must use the live owner"
        );
        source.maintain_account_group_operation_effects();
        source.maintain_account_group_operation_effects();
        source.publish_account_group_operations_for(group);
        assert_ne!(
            source.operation_publication_input(group).unwrap(),
            Some(settled_input)
        );
        assert_eq!(source.operation_projection(group).unwrap().results.len(), 3);
        let before_recall = source.operation_publication_input(group).unwrap();
        let text_event = projection
            .results
            .iter()
            .find(|result| result.reference.id == text_id)
            .unwrap()
            .message_event
            .as_ref()
            .unwrap()
            .id;
        source.recall_channel(group, text_event).await.unwrap();
        assert_ne!(
            source.operation_publication_input(group).unwrap(),
            before_recall,
            "a real recall append must invalidate source qualification"
        );
    }

    struct SettledFilePublication {
        _directory: tempfile::TempDir,
        source: std::sync::Arc<Node>,
        group: crate::eventlog::ConversationId,
        event: crate::eventlog::EventId,
        account_public: [u8; 32],
    }

    async fn settled_file_publication(expires_after_ms: Option<u64>) -> SettledFilePublication {
        let directory = tempfile::tempdir().unwrap();
        let account = crate::identity::account::Account::generate();
        let source = super::super::tests::node(&directory.path().join("source"), &account);
        let group = source
            .create_channel("File input fences", vec![])
            .await
            .unwrap();
        let path = directory.path().join("file.bin");
        std::fs::write(&path, b"real committed file").unwrap();
        let id = source
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        let expires_at_ms =
            expires_after_ms.map(|duration| crate::node::node::now_millis() + duration);
        source
            .submit_account_group_file_if(
                &account.account_id(),
                id,
                group,
                &path,
                crate::file::FileKind::File,
                expires_at_ms,
                None,
                |_| {},
                |commit| commit(),
            )
            .await
            .unwrap();
        source.maintain_account_group_operation_effects();
        source.maintain_account_group_operation_effects();
        source.publish_account_group_operations_for(group);
        let projection = source.operation_projection(group).unwrap();
        let result = projection
            .results
            .iter()
            .find(|result| result.reference.id == id)
            .unwrap();
        let event = result
            .message_event
            .as_ref()
            .expect("a real live File result")
            .id;
        assert!(result.file_descriptor.is_some());
        SettledFilePublication {
            _directory: directory,
            source,
            group,
            event,
            account_public: account.public().ed25519_pub,
        }
    }

    #[tokio::test]
    async fn settled_file_publication_rechecks_missing_rows_and_suppression() {
        for suppress in [false, true] {
            let fixture = settled_file_publication(None).await;
            let source = &fixture.source;
            let before = source
                .operation_publication_input(fixture.group)
                .unwrap()
                .unwrap();
            if suppress {
                source
                    .account_suppression
                    .lock()
                    .unwrap()
                    .suppress(fixture.group, fixture.event)
                    .unwrap();
            } else {
                assert_eq!(
                    source
                        .received_files
                        .lock()
                        .unwrap()
                        .remove_exact(fixture.group, fixture.event)
                        .unwrap(),
                    1
                );
            }
            assert_ne!(
                source.operation_publication_input(fixture.group).unwrap(),
                Some(before)
            );
            source.publish_account_group_operations_for(fixture.group);
            let key = AccountDocumentKey::GroupOperation {
                channel: hex::encode(fixture.group.as_bytes()),
                source_device: source.user_id(),
            };
            let document = source.account_document(&key).unwrap().unwrap();
            let frame = OperationFrame::decode(document.payload(), &fixture.account_public)
                .expect("the changed result is still a valid signed publication");
            assert_eq!(frame.projection.results.len(), 1);
            assert!(frame.projection.results[0].message_event.is_none());
            assert!(frame.projection.results[0].file_descriptor.is_none());
        }
    }

    #[tokio::test]
    async fn settled_file_publication_failed_repair_does_not_acknowledge_inputs() {
        let fixture = settled_file_publication(None).await;
        let source = &fixture.source;
        let key = AccountDocumentKey::GroupOperation {
            channel: hex::encode(fixture.group.as_bytes()),
            source_device: source.user_id(),
        };
        let before = source.account_documents.lock().unwrap().meta(&key).unwrap();
        {
            let mut store = source.account_documents.lock().unwrap();
            store.remove_blob_for_test(before.content_hash);
            store.expire_operation_publications_for_test();
            store.fail_index_for_test(true);
        }
        assert!(source.account_document(&key).is_err());
        source.publish_account_group_operations_for(fixture.group);
        assert_eq!(
            source.account_documents.lock().unwrap().meta(&key),
            Some(before.clone())
        );
        let reads = source
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        source
            .account_documents
            .lock()
            .unwrap()
            .fail_index_for_test(false);
        source.publish_account_group_operations_for(fixture.group);
        assert_ne!(
            source
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            reads,
            "failed repair must retry live materialization instead of acknowledging its input"
        );
        let repaired = source.account_document(&key).unwrap().unwrap();
        assert!(repaired.meta.revision > before.revision);
        let frame = OperationFrame::decode(repaired.payload(), &fixture.account_public).unwrap();
        assert!(frame.projection.results[0].message_event.is_some());
        assert!(frame.projection.results[0].file_descriptor.is_some());
    }

    #[tokio::test]
    async fn changed_file_input_during_materialization_is_not_acknowledged() {
        let fixture = settled_file_publication(None).await;
        let source = &fixture.source;
        source
            .account_documents
            .lock()
            .unwrap()
            .expire_operation_publications_for_test();
        let weak = std::sync::Arc::downgrade(source);
        let group = fixture.group;
        let event = fixture.event;
        let changed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hook_changed = changed.clone();
        *source.account_client_test_hook.lock().unwrap() =
            Some(std::sync::Arc::new(move |_, phase| {
                if phase.starts_with("operation_publication_owner ")
                    && phase.contains("phase=projection_end_store_lock_begin ")
                    && !hook_changed.swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    weak.upgrade()
                        .unwrap()
                        .account_suppression
                        .lock()
                        .unwrap()
                        .suppress(group, event)
                        .unwrap();
                }
            }));
        source.publish_account_group_operations_for(group);
        *source.account_client_test_hook.lock().unwrap() = None;
        assert!(changed.load(std::sync::atomic::Ordering::SeqCst));
        let reads = || {
            source
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts()
        };
        let before = reads();
        source.publish_account_group_operations_for(group);
        assert_ne!(
            reads(),
            before,
            "changed inputs must not earn a stale early-return qualification"
        );
        let key = AccountDocumentKey::GroupOperation {
            channel: hex::encode(group.as_bytes()),
            source_device: source.user_id(),
        };
        let document = source.account_document(&key).unwrap().unwrap();
        let frame = OperationFrame::decode(document.payload(), &fixture.account_public).unwrap();
        assert!(frame.projection.results[0].message_event.is_none());
        assert!(frame.projection.results[0].file_descriptor.is_none());
    }

    #[tokio::test]
    async fn expiring_file_publication_keeps_live_expiry() {
        let mut fixture = settled_file_publication(Some(60_000)).await;
        let source = &mut fixture.source;
        let row = source
            .received_files
            .lock()
            .unwrap()
            .header_in(&fixture.group, fixture.event)
            .unwrap()
            .unwrap();
        let deadline = row
            .deadline
            .expect("real expiry policy is persisted in the header");
        assert_eq!(
            source.operation_publication_input(fixture.group).unwrap(),
            None
        );
        assert!(
            source
                .expiry_first_seen
                .lock()
                .unwrap()
                .contains_key(&fixture.event),
            "live projection records the real first observation"
        );
        assert!(source.expiry_now() < deadline);
        // Move the policy clock forward, as the expiry owner's controls do.
        // A backward anchor cannot override the real wall clock.
        std::sync::Arc::get_mut(source).unwrap().expiry_clock_start =
            (std::time::Instant::now(), deadline + 1);
        assert_eq!(
            source.operation_publication_input(fixture.group).unwrap(),
            None
        );
        source.publish_account_group_operations_for(fixture.group);
        let key = AccountDocumentKey::GroupOperation {
            channel: hex::encode(fixture.group.as_bytes()),
            source_device: source.user_id(),
        };
        let document = source.account_document(&key).unwrap().unwrap();
        let frame = OperationFrame::decode(document.payload(), &fixture.account_public).unwrap();
        assert!(frame.projection.results[0].message_event.is_none());
        assert!(frame.projection.results[0].file_descriptor.is_none());
    }

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
        // Settle the exact already-verified ordinary and legacy heads before
        // measuring the real unchanged publication path.
        source.publish_account_group_operations();
        source.publish_account_group_legacy_operations();
        let body_reads = source.account_documents.lock().unwrap().body_read_count();
        for _ in 0..16 {
            source.publish_account_group_operations();
            source.publish_account_group_legacy_operations();
        }
        assert_eq!(
            source.account_documents.lock().unwrap().body_read_count(),
            body_reads,
            "unchanged publication must compare exact scheduling metadata before loading encrypted document blobs"
        );
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
        let before_repair = source.account_documents.lock().unwrap().body_read_count();
        source
            .account_documents
            .lock()
            .unwrap()
            .expire_operation_publications_for_test();
        source.publish_account_group_operations();
        source.publish_account_group_legacy_operations();
        assert_eq!(
            source.account_documents.lock().unwrap().body_read_count(),
            before_repair + 2,
            "sparse repair must re-read both ordinary and legacy durable heads"
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
        // A producer qualification is not a payload cache: explicit reads must
        // still report lost/corrupt storage, and sparse repair must retry a
        // failed durable publication rather than acknowledge it as settled.
        let current = source.account_documents.lock().unwrap().meta(&key).unwrap();
        {
            let mut store = source.account_documents.lock().unwrap();
            store.remove_blob_for_test(current.content_hash);
            store.expire_operation_publications_for_test();
            store.fail_index_for_test(true);
        }
        assert!(source.account_document(&key).is_err());
        source.publish_account_group_operations();
        assert_eq!(
            source.account_documents.lock().unwrap().meta(&key).as_ref(),
            Some(&current)
        );
        source
            .account_documents
            .lock()
            .unwrap()
            .fail_index_for_test(false);
        source.publish_account_group_operations();
        let repaired = source.account_document(&key).unwrap().unwrap();
        assert!(repaired.meta.revision > current.revision);
        assert!(
            OperationFrame::decode(repaired.payload(), &account.public().ed25519_pub).is_some()
        );
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
