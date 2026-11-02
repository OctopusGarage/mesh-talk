//! Explicit requested assets share the existing authenticated account session.
use super::*;
use crate::eventlog::{Event, EventKind};
use crate::node::account_documents::wire::{
    DocumentRequest, GroupFileAssetRequest, OperationProbe,
};
use crate::node::account_sync_wire::{AccountHistoryFrame, AccountSyncVersion, MAX_CHUNK};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

fn unavailable() -> NodeError {
    NodeError::File("account group file assets unavailable".into())
}

impl Node {
    /// This is a local content-validation cache, never a membership assertion.
    pub(in crate::node) fn account_group_file_assets_ready(
        &self,
        reference: &journal::OperationReference,
        payload: &GroupFilePayload,
    ) -> Result<bool, crate::eventlog::LogError> {
        Ok(reference.executor.device == self.identity.public()
            && self.account_group_file_cached_assets_ready(reference, payload)?)
    }
    pub(in crate::node) fn account_group_file_cached_assets_ready(
        &self,
        reference: &journal::OperationReference,
        payload: &GroupFilePayload,
    ) -> Result<bool, crate::eventlog::LogError> {
        if !reference.valid(&self.account.public().ed25519_pub)
            || !payload
                .descriptor
                .as_ref()
                .is_some_and(|descriptor| descriptor.valid_for(reference, &payload.logical))
        {
            return Ok(false);
        }
        let receipt = {
            let journal = self
                .group_operations
                .lock()
                .expect("group operation journal mutex");
            if journal.file_preparation(reference.id) != Some(payload) {
                return Ok(false);
            }
            journal.file_asset_receipt(reference.id).cloned()
        };
        let Some(receipt) = receipt else {
            return Ok(false);
        };
        Ok(receipt.valid_for(payload)
            && self
                .log
                .lock()
                .expect("event log mutex")
                .fingerprint_and_len(&receipt.file_conv)?
                == (receipt.fingerprint, receipt.event_count as usize))
    }
    fn group_file_executor_current(
        &self,
        reference: &journal::OperationReference,
    ) -> Result<bool, crate::eventlog::LogError> {
        if reference.executor.device != self.identity.public()
            || self.runtime_work.is_closed()
            || self
                .delivery_suspended
                .load(std::sync::atomic::Ordering::Acquire)
            || !reference.valid(&self.account.public().ed25519_pub)
            || !self.account_group_binding_current(&reference.origin)
        {
            return Ok(false);
        }
        let entry = self
            .group_operations
            .lock()
            .expect("group operation journal mutex")
            .entries
            .get(&reference.id)
            .cloned();
        let Some(entry) = entry else {
            return Ok(false);
        };
        if !entry.reference.same(reference)
            || entry.cancel
            || matches!(
                entry.status.phase,
                AccountGroupOperationPhase::Cancelled | AccountGroupOperationPhase::Rejected { .. }
            )
        {
            return Ok(false);
        }
        if let Some(event) = entry.status.primary_event
            && self
                .recalls
                .lock()
                .expect("recalls mutex")
                .contains(reference.group, event)?
        {
            return Ok(false);
        }
        let proof = self.own_group_proof();
        let projection = self.account_group_projection(reference.group)?;
        Ok(Self::public_account_group_source(
            reference.group,
            &projection,
            &self.identity.public(),
            &proof,
        )
        .is_some_and(|source| {
            matches!(
                source.role,
                AccountGroupRole::Member | AccountGroupRole::Owner
            ) && source.lifetime == reference.executor.lifetime
                && source.authority == reference.executor.authority
        }))
    }
    fn group_file_result_current(
        &self,
        reference: &journal::OperationReference,
        payload: &GroupFilePayload,
        event: &Event,
    ) -> Result<bool, crate::eventlog::LogError> {
        if self.runtime_work.is_closed()
            || self
                .delivery_suspended
                .load(std::sync::atomic::Ordering::Acquire)
            || !reference.valid(&self.account.public().ed25519_pub)
            || event.kind != EventKind::FileManifest
            || event.conversation_id != reference.group
            || event.author.ed25519_pub() != &reference.executor.device.ed25519_pub
        {
            return Ok(false);
        }
        let entry = self
            .group_operations
            .lock()
            .expect("group operation journal mutex")
            .entries
            .get(&reference.id)
            .cloned();
        if entry.is_none_or(|entry| {
            entry.source
                || !entry.reference.same(reference)
                || entry.status.phase != AccountGroupOperationPhase::Committed
                || entry.status.primary_event != Some(event.id)
                || entry
                    .file_copy
                    .as_ref()
                    .is_none_or(|copy| payload.descriptor.as_ref() != Some(&copy.descriptor))
        }) {
            return Ok(false);
        }
        if self
            .recalls
            .lock()
            .expect("recalls mutex")
            .contains(reference.group, event.id)?
        {
            return Ok(false);
        }
        let live = self
            .received_files
            .lock()
            .expect("received files mutex")
            .entry(event.id)?
            .is_some_and(|row| {
                row.conversation == reference.group
                    && row.from == reference.executor.device.user_id()
                    && payload
                        .descriptor
                        .as_ref()
                        .is_some_and(|descriptor| row.plaintext == descriptor.manifest)
            });
        Ok(live
            && self
                .account_file_proofs
                .lock()
                .expect("file proofs mutex")
                .get(&event.id)?
                .is_some())
    }
    fn group_file_fetch_current(
        &self,
        reference: &journal::OperationReference,
        payload: &GroupFilePayload,
        event: Option<&Event>,
    ) -> Result<bool, crate::eventlog::LogError> {
        if payload
            .logical
            .expires_at_ms
            .is_some_and(|time| time <= self.expiry_now())
        {
            return Ok(false);
        }
        match event {
            Some(event) => Ok(self.group_file_result_current(reference, payload, event)?
                && !self
                    .delivery
                    .lock()
                    .expect("delivery mutex")
                    .manifest_event_erased(event.id)),
            None => self.group_file_executor_current(reference),
        }
    }

    pub(in crate::node) async fn fetch_account_group_file_result_assets(
        self: &Arc<Self>,
        reference: journal::OperationReference,
        payload: GroupFilePayload,
        committed_event: Event,
    ) -> Result<(), NodeError> {
        if !committed_event.verify_integrity() || !committed_event.verify_signature() {
            return Err(unavailable());
        }
        self.fetch_group_file_assets(reference, payload, Some(committed_event))
            .await
    }

    /// Caller owns one bounded account-worker data task.
    pub(in crate::node) async fn fetch_account_group_file_assets(
        self: &Arc<Self>,
        reference: journal::OperationReference,
        payload: GroupFilePayload,
    ) -> Result<(), NodeError> {
        self.fetch_group_file_assets(reference, payload, None).await
    }

    async fn fetch_group_file_assets(
        self: &Arc<Self>,
        reference: journal::OperationReference,
        payload: GroupFilePayload,
        result_event: Option<Event>,
    ) -> Result<(), NodeError> {
        let _permit = self.runtime_work.admit().ok_or_else(unavailable)?;
        let descriptor = payload
            .descriptor
            .as_ref()
            .filter(|descriptor| descriptor.valid_for(&reference, &payload.logical))
            .ok_or_else(unavailable)?;
        let manifest =
            crate::node::files::validated_manifest(&descriptor.manifest).ok_or_else(unavailable)?;
        if !self.group_file_fetch_current(&reference, &payload, result_event.as_ref())? {
            return Err(NodeError::File(
                "file asset source scope is not current".into(),
            ));
        }
        if payload
            .logical
            .expires_at_ms
            .is_some_and(|time| time <= self.expiry_now())
        {
            return Err(unavailable());
        }
        if result_event.is_some()
            && self.account_group_file_cached_assets_ready(&reference, &payload)?
        {
            let delivery = self.delivery.lock().expect("delivery mutex");
            let event = result_event.as_ref().ok_or_else(unavailable)?;
            if delivery.manifest_event_erased(event.id)
                || !self.group_file_result_current(&reference, &payload, event)?
                || payload
                    .logical
                    .expires_at_ms
                    .is_some_and(|time| time <= self.expiry_now())
            {
                return Err(unavailable());
            }
            return self
                .group_operations
                .lock()
                .expect("group operation journal mutex")
                .complete_file_copy(&reference, descriptor)
                .map_err(NodeError::Log);
        }
        if result_event.is_none() {
            if self.account_group_file_assets_ready(&reference, &payload)? {
                return Ok(());
            }
            // Execution alone retains the independently signed original preparation.
            self.group_operations
                .lock()
                .expect("group operation journal mutex")
                .put_file_preparation(payload.clone())
                .map_err(NodeError::Log)?;
        }
        let file = manifest.file_conv();
        let ids: Vec<_> = {
            let log = self.log.lock().expect("event log mutex");
            let events = log.events(&file)?;
            let author = events.first().map(|event| event.author);
            if events
                .iter()
                .any(|event| event.kind != EventKind::Message || Some(event.author) != author)
                || result_event.is_none()
                    && author.is_some_and(|author| {
                        author.ed25519_pub() != &self.identity.public().ed25519_pub
                    })
            {
                return Err(unavailable());
            }
            events.into_iter().map(|event| event.id).collect()
        };
        if ids.len() > manifest.chunk_count() as usize {
            return Err(unavailable());
        }
        // Result content belongs in the existing private managed cache. Public
        // signed chunks may arrive independently; never compete in their DAG.
        // An existing verified prefix remains a reusable input, not a new author.
        let media_dir = if result_event.is_some() {
            Some(tempfile::tempdir().map_err(|_| unavailable())?)
        } else {
            None
        };
        let mut media_output = if let Some(dir) = &media_dir {
            Some(
                tokio::fs::File::create(dir.path().join("verified-file"))
                    .await
                    .map_err(|_| unavailable())?,
            )
        } else {
            None
        };
        let mut asset_hash = Sha256::new();
        asset_hash.update(b"mesh-talk-account-group-file-asset-v1");
        let mut plaintext_hash = Sha256::new();
        let mut plaintext_len = 0u64;
        for (index, id) in ids.iter().enumerate() {
            if index != 0 && index % 16 == 0 {
                tokio::task::yield_now().await;
            }
            if !self.group_file_fetch_current(&reference, &payload, result_event.as_ref())? {
                return Err(unavailable());
            }
            let event = self
                .log
                .lock()
                .expect("event log mutex")
                .get(id)?
                .ok_or_else(unavailable)?;
            if !event.verify_integrity() || !event.verify_signature() {
                return Err(unavailable());
            }
            let plaintext = crate::file::open_chunk_for(&manifest, index as u32, &event.ciphertext)
                .map_err(|_| unavailable())?;
            if let Some(output) = media_output.as_mut() {
                output
                    .write_all(&plaintext)
                    .await
                    .map_err(|_| unavailable())?;
            }
            plaintext_len += plaintext.len() as u64;
            plaintext_hash.update(&plaintext);
            asset_hash.update((index as u64).to_be_bytes());
            asset_hash.update((event.ciphertext.len() as u64).to_be_bytes());
            asset_hash.update(&event.ciphertext);
        }
        let mut index = ids.len() as u32;
        if index < manifest.chunk_count() {
            let origin = if result_event.is_some() {
                reference.executor.device.clone()
            } else {
                reference.origin.public()
            };
            if !self.account_group_operation_ready_for(&origin, AccountGroupOperationKind::File) {
                return Err(unavailable());
            }
            let peer = self
                .routing_peer(&origin.user_id())
                .filter(|peer| peer.public == origin)
                .ok_or_else(unavailable)?;
            let request = GroupFileAssetRequest {
                id: reference.id,
                group: reference.group,
                digest: reference.digest,
                file_conv: *file.as_bytes(),
                start_chunk: index,
                result: result_event.is_some(),
            };
            let mut nonce = [0u8; 32];
            use rand_core::RngCore;
            while nonce == [0; 32] {
                rand_core::OsRng.fill_bytes(&mut nonce);
            }
            let document = DocumentRequest {
                version: 5,
                account_id: self.account_id(),
                domain: crate::node::AccountDocumentDomain::GroupControl,
                after: None,
                known_digest: [0; 32],
                operation: Some(OperationProbe {
                    legacy_leave: false,
                    version: 1,
                    nonce,
                    after: None,
                    known_digest: [0; 32],
                    asset: Some(request),
                    personal: false,
                }),
            };
            let mut channel = self
                .begin_account_exchange(
                    &peer,
                    AccountSyncVersion::V2,
                    AccountHistoryFrame::DocumentsRequest {
                        body: serde_json::to_vec(&document).map_err(|_| unavailable())?,
                    },
                )
                .await?;
            let mut started = false;
            let mut buffer = Vec::with_capacity(crate::file::CHUNK_SIZE + 16);
            loop {
                if !self.group_file_fetch_current(&reference, &payload, result_event.as_ref())?
                    || payload
                        .logical
                        .expires_at_ms
                        .is_some_and(|time| time <= self.expiry_now())
                    || !channel.io_admitted()
                    || channel.peer_identity() != &origin
                    || !self.account_allowed(&origin, channel.peer_announcement())
                {
                    return Err(unavailable());
                }
                #[cfg(test)]
                if started {
                    let hook = self.group_file_asset_recv_test_hook.lock().unwrap().take();
                    if let Some(hook) = hook {
                        hook();
                    }
                }
                let bytes = tokio::time::timeout(Duration::from_secs(8), channel.recv())
                    .await
                    .map_err(|_| unavailable())?
                    .map_err(|_| unavailable())?;
                // The authenticated read can outlive cancellation, expiry, or
                // account/runtime retirement. Recheck before using its bytes.
                if !self.group_file_fetch_current(&reference, &payload, result_event.as_ref())?
                    || payload
                        .logical
                        .expires_at_ms
                        .is_some_and(|time| time <= self.expiry_now())
                    || !channel.io_admitted()
                    || channel.peer_identity() != &origin
                    || !self.account_allowed(&origin, channel.peer_announcement())
                {
                    return Err(unavailable());
                }
                let (version, frame) =
                    AccountHistoryFrame::decode_versioned(&bytes).ok_or_else(unavailable)?;
                if version != AccountSyncVersion::V2 {
                    return Err(unavailable());
                }
                match frame {
                    AccountHistoryFrame::AttachmentStart {
                        file_conv,
                        name,
                        size,
                        sha256,
                    } if !started
                        && file_conv == *file.as_bytes()
                        && name == "operation-file"
                        && size == payload.logical.size
                        && sha256 == descriptor.asset_ciphertext_digest =>
                    {
                        started = true;
                    }
                    AccountHistoryFrame::AttachmentChunk { bytes }
                        if started
                            && !bytes.is_empty()
                            && bytes.len() <= MAX_CHUNK
                            && index < manifest.chunk_count() =>
                    {
                        let expected_plain = (payload
                            .logical
                            .size
                            .saturating_sub(u64::from(index) * crate::file::CHUNK_SIZE as u64))
                        .min(crate::file::CHUNK_SIZE as u64)
                            as usize;
                        let expected = expected_plain + 16;
                        if buffer.len() + bytes.len() > expected {
                            return Err(unavailable());
                        }
                        buffer.extend_from_slice(&bytes);
                        if buffer.len() == expected {
                            let plaintext = crate::file::open_chunk_for(&manifest, index, &buffer)
                                .map_err(|_| unavailable())?;
                            if let Some(output) = media_output.as_mut() {
                                output
                                    .write_all(&plaintext)
                                    .await
                                    .map_err(|_| unavailable())?;
                            }
                            plaintext_len += plaintext.len() as u64;
                            plaintext_hash.update(&plaintext);
                            asset_hash.update(u64::from(index).to_be_bytes());
                            asset_hash.update((buffer.len() as u64).to_be_bytes());
                            asset_hash.update(&buffer);
                            let ciphertext = std::mem::take(&mut buffer);
                            if media_output.is_none() {
                                let append_reference = reference.clone();
                                let append_payload = payload.clone();
                                let append_result = result_event.clone();
                                self.account_blocking(move |node| {
                                    node.append_event_if(
                                        file,
                                        EventKind::Message,
                                        ciphertext,
                                        |delivery| {
                                            let current = match append_result.as_ref() {
                                                Some(event) => {
                                                    !delivery.manifest_event_erased(event.id)
                                                        && node.group_file_result_current(
                                                            &append_reference,
                                                            &append_payload,
                                                            event,
                                                        )?
                                                }
                                                None => node.group_file_executor_current(
                                                    &append_reference,
                                                )?,
                                            };
                                            if !current
                                                || append_payload
                                                    .logical
                                                    .expires_at_ms
                                                    .is_some_and(|time| time <= node.expiry_now())
                                            {
                                                return Err(unavailable());
                                            }
                                            Ok(())
                                        },
                                    )
                                    .map(|_| ())
                                })
                                .await?;
                            }
                            index += 1;
                        }
                    }
                    AccountHistoryFrame::AttachmentComplete
                        if started && index == manifest.chunk_count() && buffer.is_empty() =>
                    {
                        channel.recycle(self);
                        break;
                    }
                    _ => return Err(unavailable()),
                }
            }
        }
        let asset_digest: [u8; 32] = asset_hash.finalize().into();
        let checksum: [u8; 32] = plaintext_hash.finalize().into();
        if plaintext_len != payload.logical.size
            || checksum != payload.logical.checksum
            || asset_digest != descriptor.asset_ciphertext_digest
        {
            return Err(unavailable());
        }
        if !self.group_file_fetch_current(&reference, &payload, result_event.as_ref())? {
            return Err(unavailable());
        }
        if let Some(output) = media_output.take() {
            output.sync_all().await.map_err(|_| unavailable())?;
        }
        let node = Arc::clone(self);
        self.account_blocking(move |_| {
            let prepared = if let Some(dir) = media_dir {
                Some(
                    node.media
                        .prepare_from_path_if(
                            file,
                            &payload.logical.name,
                            &dir.path().join("verified-file"),
                            || {
                                if node
                                    .group_file_fetch_current(
                                        &reference,
                                        &payload,
                                        result_event.as_ref(),
                                    )
                                    .map_err(std::io::Error::other)?
                                {
                                    Ok(())
                                } else {
                                    Err(std::io::Error::other(
                                        "file copy scope retired during preparation",
                                    ))
                                }
                            },
                        )
                        .map_err(|_| unavailable())?,
                )
            } else {
                None
            };
            let delivery = node.delivery.lock().expect("delivery mutex");
            let current = match result_event.as_ref() {
                Some(event) => {
                    !delivery.manifest_event_erased(event.id)
                        && node.group_file_result_current(&reference, &payload, event)?
                }
                None => node.group_file_executor_current(&reference)?,
            };
            if !current
                || payload
                    .logical
                    .expires_at_ms
                    .is_some_and(|time| time <= node.expiry_now())
            {
                return Err(unavailable());
            }
            let (fingerprint, count) = {
                let log = node.log.lock().expect("event log mutex");
                log.sync().map_err(NodeError::Log)?;
                log.fingerprint_and_len(&file)?
            };
            if result_event.is_some() {
                if let Some(prepared) = prepared {
                    node.media
                        .publish_prepared(prepared)
                        .map_err(|_| unavailable())?;
                }
                return node
                    .group_operations
                    .lock()
                    .expect("group operation journal mutex")
                    .complete_file_copy(
                        &reference,
                        payload.descriptor.as_ref().ok_or_else(unavailable)?,
                    )
                    .map_err(NodeError::Log);
            }
            let receipt = GroupFileAssetsReceipt {
                file_conv: file,
                fingerprint,
                event_count: count.try_into().map_err(|_| unavailable())?,
                asset_digest,
            };
            node.group_operations
                .lock()
                .expect("group operation journal mutex")
                .put_file_asset_receipt(payload, receipt)
                .map_err(NodeError::Log)
        })
        .await
    }
}
