//! ID-bound local file preparation for an account-owned group intent.
use super::*;
mod assets;
use crate::{file::FileKind, node::FileProgress};
use std::{path::Path, sync::Arc};

impl Node {
    #[cfg(test)]
    pub(in crate::node) fn file_copy_complete_for_test(
        &self,
        id: AccountGroupOperationId,
    ) -> Option<bool> {
        self.group_operations
            .lock()
            .unwrap()
            .entries
            .get(&id)
            .and_then(|entry| entry.file_copy.as_ref())
            .map(|copy| copy.complete)
    }
    #[cfg(test)]
    pub(in crate::node) fn install_original_file_request_for_test(
        &self,
        reference: journal::OperationReference,
        body: AccountGroupOperation,
    ) -> Result<(), NodeError> {
        let status = AccountGroupOperationStatus {
            id: reference.id,
            account_id: self.account_id(),
            group: reference.group,
            kind: AccountGroupOperationKind::File,
            phase: AccountGroupOperationPhase::AwaitingAcceptance,
            executions: vec![],
            committed_events: vec![],
            primary_event: None,
        };
        self.group_operations
            .lock()
            .expect("group operation journal mutex")
            .put(journal::Entry {
                reference,
                body: Some(body),
                status,
                source: false,
                file_copy: None,
                cancel: false,
                sequence: 0,
            })
            .map_err(NodeError::Log)
    }
    pub(in crate::node) fn group_operation_asset_payload(
        &self,
        request: &crate::node::account_documents::wire::GroupFileAssetRequest,
        executor: &crate::identity::device::PublicIdentity,
    ) -> Option<GroupFilePayload> {
        if !request.valid()
            || self.runtime_work.is_closed()
            || self
                .delivery_suspended
                .load(std::sync::atomic::Ordering::Acquire)
            || !self.certified_sibling(executor)
        {
            return None;
        }
        let proof = self.historical_author(&executor.ed25519_pub)?;
        if proof.public() != *executor
            || proof
                .account_cert
                .as_ref()
                .is_none_or(|cert| cert.account_ed25519_pub != self.account.public().ed25519_pub)
            || !self.account_group_binding_current(&proof)
        {
            return None;
        }
        let (entry, preparation) = {
            let journal = self
                .group_operations
                .lock()
                .expect("group operation journal mutex");
            (
                journal.entries.get(&request.id)?.clone(),
                journal.file_preparation(request.id).cloned(),
            )
        };
        let reference = &entry.reference;
        if (!request.result && entry.cancel)
            || reference.kind != AccountGroupOperationKind::File
            || reference.group != request.group
            || reference.digest != request.digest
            || (!request.result && reference.executor.device != *executor)
            || !reference.valid(&self.account.public().ed25519_pub)
            || matches!(
                entry.status.phase,
                AccountGroupOperationPhase::Cancelled | AccountGroupOperationPhase::Rejected { .. }
            )
        {
            return None;
        }
        if request.result {
            if !entry.source
                || entry.status.phase != AccountGroupOperationPhase::Committed
                || reference.executor.device != self.identity.public()
            {
                return None;
            }
            let id = entry.status.primary_event?;
            if self
                .delivery
                .lock()
                .expect("delivery mutex")
                .manifest_event_erased(id)
            {
                return None;
            }
            let event = self
                .log
                .lock()
                .expect("event log mutex")
                .get(&id)
                .cloned()?;
            if event.kind != crate::eventlog::EventKind::FileManifest
                || event.conversation_id != reference.group
                || event.author.ed25519_pub() != &reference.executor.device.ed25519_pub
                || !event.verify_integrity()
                || !event.verify_signature()
            {
                return None;
            }
        }
        let preparation = preparation.or_else(|| {
            entry.file_copy.as_ref().and_then(|copy| {
                Some(GroupFilePayload {
                    logical: GroupFileLogical::from_manifest(&copy.descriptor.manifest)?,
                    descriptor: Some(copy.descriptor.clone()),
                })
            })
        });
        let payload = preparation.or_else(|| match entry.body.as_ref()? {
            AccountGroupOperation::Send { message } => {
                GroupFilePayload::decode(message.encoded_body())
            }
            _ => None,
        })?;
        let descriptor = payload.descriptor.as_ref()?;
        if !descriptor.valid_for(reference, &payload.logical)
            || payload
                .logical
                .expires_at_ms
                .is_some_and(|deadline| deadline <= self.expiry_now())
        {
            return None;
        }
        let manifest = crate::node::files::validated_manifest(&descriptor.manifest)?;
        if manifest.file_conv().as_bytes() != &request.file_conv
            || request.start_chunk > manifest.chunk_count()
        {
            return None;
        }
        if entry.status.primary_event.is_some_and(|event| {
            self.recalls
                .lock()
                .expect("recalls mutex")
                .targets(&request.group)
                .contains(&event)
        }) {
            return None;
        }
        Some(payload)
    }
    pub(in crate::node) async fn serve_group_operation_asset(
        self: &Arc<Self>,
        channel: &mut crate::transport::SecureChannel<tokio::net::TcpStream>,
        request: &crate::node::account_documents::wire::GroupFileAssetRequest,
    ) -> bool {
        use crate::node::account_sync_wire::{AccountHistoryFrame, AccountSyncVersion, MAX_CHUNK};
        let peer = channel.peer_identity().clone();
        let Some(payload) = self.group_operation_asset_payload(request, &peer) else {
            return false;
        };
        let descriptor = payload.descriptor.as_ref().expect("validated descriptor");
        let Some(manifest) = crate::node::files::validated_manifest(&descriptor.manifest) else {
            return false;
        };
        let file = manifest.file_conv();
        let ids: Vec<_> = {
            let log = self.log.lock().expect("event log mutex");
            log.events(&file)
                .into_iter()
                .filter(|event| event.kind == crate::eventlog::EventKind::Message)
                .map(|event| event.id)
                .collect()
        };
        if ids.len() != manifest.chunk_count() as usize {
            return false;
        }
        let (_, revision) = self.device_sync_policy_snapshot();
        let start = AccountHistoryFrame::AttachmentStart {
            file_conv: request.file_conv,
            name: "operation-file".into(),
            size: payload.logical.size,
            sha256: descriptor.asset_ciphertext_digest,
        };
        for frame in [start] {
            let _disclosure = self.privacy.gate.read().await;
            if !channel.io_admitted()
                || !self.account_allowed(&peer, channel.peer_announcement())
                || self.group_operation_asset_payload(request, &peer).as_ref() != Some(&payload)
                || self
                    .send_account_frame(channel, AccountSyncVersion::V2, revision, &frame)
                    .await
                    .is_err()
            {
                return false;
            }
        }
        for (index, id) in ids.iter().enumerate().skip(request.start_chunk as usize) {
            let ciphertext = {
                let log = self.log.lock().expect("event log mutex");
                let Some(event) = log.get(id) else {
                    return false;
                };
                if !event.verify_integrity() || !event.verify_signature() {
                    return false;
                }
                event.ciphertext.clone()
            };
            if crate::file::open_chunk_for(&manifest, index as u32, &ciphertext).is_err() {
                return false;
            }
            for bytes in ciphertext.chunks(MAX_CHUNK) {
                let _disclosure = self.privacy.gate.read().await;
                if !channel.io_admitted()
                    || !self.account_allowed(&peer, channel.peer_announcement())
                    || self.group_operation_asset_payload(request, &peer).as_ref() != Some(&payload)
                    || self
                        .send_account_frame(
                            channel,
                            AccountSyncVersion::V2,
                            revision,
                            &AccountHistoryFrame::AttachmentChunk {
                                bytes: bytes.to_vec(),
                            },
                        )
                        .await
                        .is_err()
                {
                    return false;
                }
            }
        }
        let _disclosure = self.privacy.gate.read().await;
        channel.io_admitted()
            && self.account_allowed(&peer, channel.peer_announcement())
            && self.group_operation_asset_payload(request, &peer).as_ref() == Some(&payload)
            && self
                .send_account_frame(
                    channel,
                    AccountSyncVersion::V2,
                    revision,
                    &AccountHistoryFrame::AttachmentComplete,
                )
                .await
                .is_ok()
    }
    pub async fn submit_account_group_file_if(
        self: &Arc<Self>,
        expected_account: &str,
        id: AccountGroupOperationId,
        group: ConversationId,
        path: &Path,
        kind: FileKind,
        expires_at_ms: Option<u64>,
        protection: Option<crate::node::MessageProtection>,
        on_progress: impl FnMut(FileProgress) + Send + 'static,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<AccountGroupOperationStatus, NodeError> {
        self.ensure_group_operation_account(expected_account)?;
        authorize(&mut || Ok(()))?;
        let payload = self
            .prepare_account_group_file(
                expected_account,
                id,
                group,
                path,
                kind,
                expires_at_ms,
                protection,
                on_progress,
            )
            .await?;
        let message = payload
            .prepared()
            .ok_or_else(|| NodeError::InvalidInput("invalid prepared group file".into()))?;
        self.submit_account_group_operation_if(
            expected_account,
            id,
            group,
            AccountGroupOperation::Send { message },
            |commit| authorize(commit),
        )
    }

    /// The owned job retains both runtime admission and the preparation gate
    /// through the durable, nonexecutable artifact write, even if its caller exits.
    pub(in crate::node) async fn prepare_account_group_file(
        self: &Arc<Self>,
        expected_account: &str,
        id: AccountGroupOperationId,
        group: ConversationId,
        path: &Path,
        kind: FileKind,
        expires_at_ms: Option<u64>,
        protection: Option<crate::node::MessageProtection>,
        on_progress: impl FnMut(FileProgress) + Send + 'static,
    ) -> Result<GroupFilePayload, NodeError> {
        self.ensure_group_operation_account(expected_account)?;
        let permit = self
            .runtime_work
            .admit()
            .ok_or_else(|| NodeError::Authorization("participant runtime is stopping".into()))?;
        let gate = Arc::clone(&self.group_file_prepare_gate).lock_owned().await;
        let node = Arc::clone(self);
        let expected_account = expected_account.to_owned();
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _gate = gate;
            node.ensure_group_operation_account(&expected_account)?;
            if node.runtime_work.is_closed()
                || node
                    .delivery_suspended
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(NodeError::Authorization(
                    "participant runtime is stopping".into(),
                ));
            }
            if protection.is_some() {
                crate::node::files::validate_protected_image_path(&path, kind)?;
            }
            let directory = if path.is_dir() {
                Some(crate::file::directory::pack(&path)?)
            } else {
                None
            };
            let source_path = directory
                .as_ref()
                .map_or(path.as_path(), |archive| archive.path());
            let size = std::fs::metadata(source_path)
                .map_err(|_| NodeError::File("file is unavailable".into()))?
                .len();
            if size > crate::node::node::MAX_FILE_SIZE {
                return Err(NodeError::InvalidInput("file too large".into()));
            }
            let name = if directory.is_some() {
                crate::file::directory::archive_name(&path)?
            } else if protection.is_some() {
                format!(
                    "protected-image.{}",
                    path.extension()
                        .and_then(|ext| ext.to_str())
                        .unwrap_or("img")
                )
            } else {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .ok_or_else(|| NodeError::InvalidInput("file name unavailable".into()))?
            };
            let mime = if directory.is_some() {
                crate::file::directory::MIME.to_owned()
            } else {
                crate::node::files::mime_from_name(&name)
            };
            let kind = if directory.is_some() {
                FileKind::File
            } else {
                kind
            };
            let mut reader = std::io::BufReader::new(
                std::fs::File::open(source_path)
                    .map_err(|_| NodeError::File("file is unavailable".into()))?,
            );
            let mut digest = Sha256::new();
            let mut bytes_read = 0u64;
            let mut buffer = [0u8; 64 * 1024];
            loop {
                if node.runtime_work.is_closed() {
                    return Err(NodeError::Authorization(
                        "participant runtime is stopping".into(),
                    ));
                }
                let count = std::io::Read::read(&mut reader, &mut buffer)
                    .map_err(|_| NodeError::File("file could not be read".into()))?;
                if count == 0 {
                    break;
                }
                bytes_read += count as u64;
                if bytes_read > size {
                    return Err(NodeError::File("file changed while preparing".into()));
                }
                digest.update(&buffer[..count]);
            }
            if bytes_read != size {
                return Err(NodeError::File("file changed while preparing".into()));
            }
            let logical = GroupFileLogical {
                name,
                mime,
                size,
                checksum: digest.finalize().into(),
                kind,
                expires_at_ms,
                protection,
            };
            let logical_payload = GroupFilePayload {
                logical,
                descriptor: None,
            };
            let logical_operation = logical_payload
                .logical_operation()
                .ok_or_else(|| NodeError::InvalidInput("invalid file metadata".into()))?;
            let logical_digest = journal::operation_digest(group, &logical_operation);
            {
                let journal = node
                    .group_operations
                    .lock()
                    .expect("group operation journal mutex");
                if let Some(original) = journal.file_preparation(id) {
                    let descriptor = original.descriptor.as_ref().expect("validated preparation");
                    if descriptor.group != group || descriptor.logical_digest != logical_digest {
                        return Err(NodeError::InvalidInput(
                            "operation ID already binds different input".into(),
                        ));
                    }
                    return Ok(original.clone());
                }
                if let Some(entry) = journal.entries.get(&id) {
                    if entry.reference.group != group || entry.reference.digest != logical_digest {
                        return Err(NodeError::InvalidInput(
                            "operation ID already binds different input".into(),
                        ));
                    }
                    if let Some(AccountGroupOperation::Send { message }) = &entry.body
                        && let Some(payload) = GroupFilePayload::decode(message.encoded_body())
                    {
                        return Ok(payload);
                    }
                    if entry.terminal() {
                        return Ok(logical_payload);
                    }
                    return Err(NodeError::File(
                        "original file preparation unavailable".into(),
                    ));
                }
            }
            // A foreign-origin retry may query/adopt the original immutable
            // request, but must never stage a second randomized preparation.
            if !id.matches_origin(&node.identity.public()) {
                return Ok(logical_payload);
            }
            crate::node::expiry_policy::validate_outgoing(expires_at_ms)?;
            node.group_operations
                .lock()
                .expect("group operation journal mutex")
                .preflight_file_preparation()
                .map_err(NodeError::Log)?;
            let (mut manifest, file_conv) = node.stage_file(source_path, kind, on_progress)?;
            let mut unaccepted = UnacceptedGroupFile {
                node: Arc::clone(&node),
                file_conv,
                retained: false,
            };
            manifest.v2.name = logical_payload.logical.name.clone();
            manifest.v2.mime = logical_payload.logical.mime.clone();
            if protection.is_some() {
                node.validate_staged_protected_image(&manifest)?;
            }
            let encoded = crate::expiry::wrap(
                &crate::node::files::protected_manifest_bytes(&manifest, protection),
                expires_at_ms,
            );
            if GroupFileLogical::from_manifest(&encoded).as_ref() != Some(&logical_payload.logical)
            {
                return Err(NodeError::File("file changed while preparing".into()));
            }
            let ids: Vec<_> = {
                let log = node.log.lock().expect("event log mutex");
                log.events(&file_conv)
                    .into_iter()
                    .filter(|event| event.kind == crate::eventlog::EventKind::Message)
                    .map(|event| event.id)
                    .collect()
            };
            let mut asset_hash = Sha256::new();
            asset_hash.update(b"mesh-talk-account-group-file-asset-v1");
            for (index, id) in ids.iter().enumerate() {
                let ciphertext = node
                    .log
                    .lock()
                    .expect("event log mutex")
                    .get(id)
                    .ok_or_else(|| NodeError::File("prepared file chunk unavailable".into()))?
                    .ciphertext
                    .clone();
                asset_hash.update((index as u64).to_be_bytes());
                asset_hash.update((ciphertext.len() as u64).to_be_bytes());
                asset_hash.update(&ciphertext);
            }
            let asset_digest = asset_hash.finalize().into();
            let descriptor =
                GroupFileDescriptor::new(&node, id, group, logical_digest, encoded, asset_digest);
            let payload = GroupFilePayload {
                logical: logical_payload.logical,
                descriptor: Some(descriptor),
            }
            .compact_for_bound(&node)?;
            {
                let _delivery = node.delivery.lock().expect("delivery mutex");
                node.ensure_group_operation_account(&expected_account)?;
                if node.runtime_work.is_closed()
                    || node
                        .delivery_suspended
                        .load(std::sync::atomic::Ordering::Acquire)
                {
                    return Err(NodeError::Authorization(
                        "participant runtime is stopping".into(),
                    ));
                }
                let (fingerprint, count) = {
                    let log = node.log.lock().expect("event log mutex");
                    log.sync().map_err(NodeError::Log)?;
                    log.fingerprint_and_len(&file_conv)
                };
                let receipt = GroupFileAssetsReceipt {
                    file_conv,
                    fingerprint,
                    event_count: count
                        .try_into()
                        .map_err(|_| NodeError::File("too many prepared file chunks".into()))?,
                    asset_digest,
                };
                node.group_operations
                    .lock()
                    .expect("group operation journal mutex")
                    .put_file_asset_receipt(payload.clone(), receipt)
                    .map_err(NodeError::Log)?;
                unaccepted.retained = true;
            }
            Ok(payload)
        })
        .await
        .map_err(|_| NodeError::File("file preparation task failed".into()))?
    }
}

struct UnacceptedGroupFile {
    node: Arc<Node>,
    file_conv: ConversationId,
    retained: bool,
}
impl Drop for UnacceptedGroupFile {
    fn drop(&mut self) {
        if !self.retained {
            let _ = self
                .node
                .log
                .lock()
                .expect("event log mutex")
                .drop_conversation(&self.file_conv);
            let _ = self.node.media.remove(self.file_conv);
        }
    }
}

use crate::{discovery::Announce, identity::device::DeviceIdentity};
use bincode::Options;
use sha2::{Digest, Sha256};

const FILE_PAYLOAD_MAGIC: &[u8; 4] = b"MGF1";
// Prepared messages are JSON byte arrays on the private channel. Retain the
// existing 256 KiB body limit even at the worst four-byte encoding per byte.
const MAX_FILE_PAYLOAD: usize = 60 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::node) struct GroupFileLogical {
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub checksum: [u8; 32],
    pub kind: FileKind,
    pub expires_at_ms: Option<u64>,
    pub protection: Option<crate::node::MessageProtection>,
}
impl GroupFileLogical {
    fn valid(&self) -> bool {
        !self.name.is_empty()
            && self.name.len() <= 1024
            && !self.mime.is_empty()
            && self.mime.len() <= 256
            && self.size <= crate::node::node::MAX_FILE_SIZE
    }
    pub(in crate::node) fn from_manifest(encoded: &[u8]) -> Option<Self> {
        let manifest = crate::node::files::validated_manifest(encoded)?;
        let logical = Self {
            name: manifest.name().to_owned(),
            mime: manifest.mime().to_owned(),
            size: manifest.size(),
            checksum: manifest.checksum(),
            kind: manifest.kind()?,
            expires_at_ms: crate::expiry::deadline(encoded),
            protection: manifest.protection(),
        };
        logical.valid().then_some(logical)
    }
}

/// Independent preparation provenance survives an authenticated sibling relay.
/// This commits the original random encryption parameters without putting those
/// parameters into the logical operation digest used by raw-input retries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::node) struct GroupFileDescriptor {
    pub id: AccountGroupOperationId,
    pub group: ConversationId,
    pub logical_digest: [u8; 32],
    pub creator: Announce,
    pub manifest: Vec<u8>,
    pub asset_ciphertext_digest: [u8; 32],
    pub signature: Vec<u8>,
}
impl GroupFileDescriptor {
    pub(super) fn message(&self) -> Vec<u8> {
        let mut bytes = b"mesh-talk-account-group-file-preparation-v1".to_vec();
        bytes.extend(
            serde_json::to_vec(&(
                self.id,
                self.group,
                self.logical_digest,
                &self.creator,
                &self.manifest,
                self.asset_ciphertext_digest,
            ))
            .expect("file preparation signature payload"),
        );
        bytes
    }
    pub(in crate::node) fn new(
        node: &Node,
        id: AccountGroupOperationId,
        group: ConversationId,
        logical_digest: [u8; 32],
        manifest: Vec<u8>,
        asset_ciphertext_digest: [u8; 32],
    ) -> Self {
        let mut descriptor = Self {
            id,
            group,
            logical_digest,
            creator: node.own_group_proof(),
            manifest,
            asset_ciphertext_digest,
            signature: Vec::new(),
        };
        descriptor.signature = node.identity.sign(&descriptor.message()).to_vec();
        descriptor
    }
    pub(in crate::node) fn valid_for(
        &self,
        reference: &journal::OperationReference,
        logical: &GroupFileLogical,
    ) -> bool {
        let Ok(signature) = <[u8; 64]>::try_from(self.signature.as_slice()) else {
            return false;
        };
        reference.kind == AccountGroupOperationKind::File
            && self.id == reference.id
            && self.group == reference.group
            && self.logical_digest == reference.digest
            && self.creator.public() == reference.origin.public()
            && self.creator.account_cert == reference.origin.account_cert
            && self.creator.verify()
            && !self.creator.post_office
            && DeviceIdentity::verify(&self.creator.ed25519_pub, &self.message(), &signature)
            && GroupFileLogical::from_manifest(&self.manifest).as_ref() == Some(logical)
    }
}

/// Local validation cache only; never exported in an operation head.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::node) struct GroupFileAssetsReceipt {
    pub file_conv: ConversationId,
    pub fingerprint: [u8; 32],
    pub event_count: u32,
    pub asset_digest: [u8; 32],
}
impl GroupFileAssetsReceipt {
    pub(in crate::node) fn valid_for(&self, payload: &GroupFilePayload) -> bool {
        payload
            .descriptor
            .as_ref()
            .and_then(|descriptor| {
                crate::node::files::validated_manifest(&descriptor.manifest).map(|manifest| {
                    self.file_conv == manifest.file_conv()
                        && self.event_count == manifest.chunk_count()
                        && self.asset_digest == descriptor.asset_ciphertext_digest
                        && self.fingerprint != [0; 32]
                })
            })
            .unwrap_or(false)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::node) struct GroupFilePayload {
    pub logical: GroupFileLogical,
    pub descriptor: Option<GroupFileDescriptor>,
}
impl GroupFilePayload {
    fn compact_for_bound(mut self, node: &Node) -> Result<Self, NodeError> {
        if self.encode().is_some() {
            return Ok(self);
        }
        let descriptor = self
            .descriptor
            .as_ref()
            .ok_or_else(|| NodeError::File("file preparation exceeds transport bound".into()))?;
        if descriptor.creator.public() != node.identity.public() {
            return Err(NodeError::Authorization(
                "original file preparation belongs to another signer".into(),
            ));
        }
        let parsed = crate::node::files::validated_manifest(&descriptor.manifest)
            .ok_or_else(|| NodeError::File("invalid prepared manifest".into()))?;
        let crate::file::AnyManifest::V3(original) = parsed.unprotected() else {
            return Err(NodeError::File(
                "file preparation exceeds transport bound".into(),
            ));
        };
        let mut compact = original.clone();
        compact.v2.chunk_hashes.clear();
        let encoded = crate::expiry::wrap(
            &crate::node::files::protected_manifest_bytes(&compact, self.logical.protection),
            self.logical.expires_at_ms,
        );
        if GroupFileLogical::from_manifest(&encoded).as_ref() != Some(&self.logical) {
            return Err(NodeError::File("compact file metadata changed".into()));
        }
        self.descriptor = Some(GroupFileDescriptor::new(
            node,
            descriptor.id,
            descriptor.group,
            descriptor.logical_digest,
            encoded,
            descriptor.asset_ciphertext_digest,
        ));
        self.encode()
            .ok_or_else(|| NodeError::File("file preparation exceeds transport bound".into()))?;
        Ok(self)
    }
    pub(in crate::node) fn encode(&self) -> Option<Vec<u8>> {
        if !self.logical.valid() {
            return None;
        }
        let mut bytes = FILE_PAYLOAD_MAGIC.to_vec();
        bytes.extend(
            bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .with_limit(MAX_FILE_PAYLOAD as u64)
                .serialize(self)
                .ok()?,
        );
        (bytes.len() <= MAX_FILE_PAYLOAD).then_some(bytes)
    }
    pub(in crate::node) fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_FILE_PAYLOAD {
            return None;
        }
        let payload: Self = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(MAX_FILE_PAYLOAD as u64)
            .reject_trailing_bytes()
            .deserialize(bytes.strip_prefix(FILE_PAYLOAD_MAGIC)?)
            .ok()?;
        (payload.encode().as_deref() == Some(bytes)).then_some(payload)
    }
    pub(in crate::node) fn prepared(&self) -> Option<PreparedAccountGroupMessage> {
        Some(PreparedAccountGroupMessage {
            kind: AccountGroupOperationKind::File,
            encoded_body: self.encode()?,
            staged_assets: vec![self.logical.checksum],
        })
    }
    pub(in crate::node) fn logical_operation(&self) -> Option<AccountGroupOperation> {
        Some(AccountGroupOperation::Send {
            message: Self {
                logical: self.logical.clone(),
                descriptor: None,
            }
            .prepared()?,
        })
    }
}

/// A root over exact indexed ciphertext with explicit lengths, not private
/// staging EventIds: a sibling can reproduce the same original encrypted asset.
#[cfg(test)]
pub(in crate::node) fn group_file_asset_digest<'a>(
    chunks: impl IntoIterator<Item = &'a [u8]>,
) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"mesh-talk-account-group-file-asset-v1");
    for (index, chunk) in chunks.into_iter().enumerate() {
        hash.update((index as u64).to_be_bytes());
        hash.update((chunk.len() as u64).to_be_bytes());
        hash.update(chunk);
    }
    hash.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        channel::ChannelMeta,
        discovery::Roster,
        eventlog::{Event, EventKind},
        file::{
            CHUNK_SIZE, FileKey, FileManifestV2, FileManifestV3, chunk_hash, file_checksum,
            seal_chunk_indexed,
        },
        identity::account::Account,
    };
    use std::sync::Mutex;
    use tokio::sync::mpsc;

    fn fixture() -> (
        tempfile::TempDir,
        Arc<Node>,
        Account,
        ConversationId,
        GroupFileLogical,
        Vec<u8>,
        Vec<u8>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let (dm, _) = mpsc::unbounded_channel();
        let (channels, _) = mpsc::unbounded_channel();
        let (files, _) = mpsc::unbounded_channel();
        let node = Node::open_with_account(
            DeviceIdentity::generate(),
            Account::from_secret_bytes(account.secret_bytes()),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channels,
            files,
            &dir.path().join("events"),
            &dir.path().join("sent"),
            "pw",
        )
        .unwrap();
        let group = crate::channel::new_channel_id();
        let genesis = Event::new(
            &node.identity,
            group,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "File descriptor".into(),
                members: vec![node.identity.public()],
                epoch: 0,
                owner: node.user_id(),
            }
            .encode(),
        );
        node.log.lock().unwrap().append_durable(genesis).unwrap();
        node.process_channel(group);
        let bytes = b"original prepared bytes";
        let manifest = FileManifestV3 {
            v2: FileManifestV2 {
                name: "file.bin".into(),
                size: bytes.len() as u64,
                mime: "application/octet-stream".into(),
                checksum: file_checksum(bytes),
                file_key: [7; 32],
                file_nonce: [8; 8],
                file_conv: ConversationId::new([9; 32]),
                chunk_size: CHUNK_SIZE as u32,
                chunk_count: 1,
                chunk_hashes: vec![chunk_hash(bytes)],
            },
            kind: FileKind::File,
        }
        .encode();
        let logical = GroupFileLogical::from_manifest(&manifest).unwrap();
        let chunk = seal_chunk_indexed(&FileKey::from_bytes([7; 32]), &[8; 8], 0, bytes).unwrap();
        (dir, node, account, group, logical, manifest, chunk)
    }
    fn reference(
        node: &Node,
        group: ConversationId,
        logical: &GroupFileLogical,
    ) -> journal::OperationReference {
        let operation = GroupFilePayload {
            logical: logical.clone(),
            descriptor: None,
        }
        .logical_operation()
        .unwrap();
        journal::OperationReference::new(
            node,
            node.new_account_group_operation_id(&node.account_id())
                .unwrap(),
            group,
            &operation,
            node.account_group_access(group)
                .unwrap()
                .sources
                .into_iter()
                .next()
                .unwrap(),
        )
    }
    #[tokio::test]
    async fn raw_file_preparation_cannot_publish_ready_before_ciphertext_sync() {
        let (dir, node, account, group, _, _, _) = fixture();
        let path = dir.path().join("sync.bin");
        std::fs::write(&path, b"ciphertext must be durable first").unwrap();
        let id = node
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        node.log.lock().unwrap().fail_sync_for_test(true);
        let result = node
            .prepare_account_group_file(
                &account.account_id(),
                id,
                group,
                &path,
                FileKind::File,
                None,
                None,
                |_| {},
            )
            .await;
        assert!(
            result.is_err(),
            "ciphertext sync failure must prevent a ready receipt"
        );
        let journal = node.group_operations.lock().unwrap();
        assert!(journal.file_asset_receipt(id).is_none());
        assert!(journal.file_preparation(id).is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_raw_file_retries_and_abandoned_call_keep_one_original_preparation() {
        let (dir, node, account, group, _logical, _manifest, _chunk) = fixture();
        let path = dir.path().join("retry.bin");
        std::fs::write(&path, b"same explicit attachment").unwrap();
        let id = node
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker_node = Arc::clone(&node);
        let worker_path = path.clone();
        let account_id = account.account_id();
        let worker_account = account_id.clone();
        let mut started_tx = Some(started_tx);
        let first = tokio::spawn(async move {
            worker_node
                .prepare_account_group_file(
                    &worker_account,
                    id,
                    group,
                    &worker_path,
                    FileKind::File,
                    None,
                    None,
                    move |_| {
                        if let Some(tx) = started_tx.take() {
                            let _ = tx.send(());
                            let _ = release_rx.recv();
                        }
                    },
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), started_rx)
            .await
            .unwrap()
            .unwrap();
        first.abort();
        let retry_node = Arc::clone(&node);
        let retry_path = path.clone();
        let retry_account = account_id.clone();
        let mut retry = tokio::spawn(async move {
            retry_node
                .prepare_account_group_file(
                    &retry_account,
                    id,
                    group,
                    &retry_path,
                    FileKind::File,
                    None,
                    None,
                    |_| {},
                )
                .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut retry)
                .await
                .is_err(),
            "abandoning the caller must not release the actual writer's preparation gate"
        );
        release_tx.send(()).unwrap();
        let prepared = tokio::time::timeout(std::time::Duration::from_secs(3), retry)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let original = node
            .group_operations
            .lock()
            .unwrap()
            .file_preparation(id)
            .cloned()
            .unwrap();
        assert!(prepared == original);
        let conv =
            crate::node::files::validated_manifest(&prepared.descriptor.as_ref().unwrap().manifest)
                .unwrap()
                .file_conv();
        let before = node.log.lock().unwrap().conversations();
        let duplicate = node
            .prepare_account_group_file(
                &account_id,
                id,
                group,
                &path,
                FileKind::File,
                None,
                None,
                |_| {},
            )
            .await
            .unwrap();
        assert!(duplicate == original);
        assert_eq!(node.log.lock().unwrap().conversations(), before);
        assert_eq!(node.log.lock().unwrap().conversation_len(&conv), 1);
        assert!(node.group_operations.lock().unwrap().entries.is_empty());
        std::fs::write(&path, b"different explicit attachment").unwrap();
        assert!(
            node.prepare_account_group_file(
                &account_id,
                id,
                group,
                &path,
                FileKind::File,
                None,
                None,
                |_| {}
            )
            .await
            .is_err()
        );
        assert!(node.group_operations.lock().unwrap().file_preparation(id) == Some(&original));
    }
    #[test]
    fn original_preparation_is_durable_before_admission_and_immutable_after_reopen() {
        let (dir, node, account, group, logical, manifest, chunk) = fixture();
        let reference = reference(&node, group, &logical);
        let descriptor = GroupFileDescriptor::new(
            &node,
            reference.id,
            group,
            reference.digest,
            manifest,
            group_file_asset_digest([chunk.as_slice()]),
        );
        let payload = GroupFilePayload {
            logical,
            descriptor: Some(descriptor),
        };
        let path = dir.path().join("preparation-journal");
        std::fs::create_dir(&path).unwrap();
        let open = || {
            journal::OperationJournal::open(
                &path,
                "pw",
                account.public().ed25519_pub,
                node.identity.public(),
            )
            .unwrap()
        };
        let mut journal = open();
        journal
            .put_file_preparation(payload.clone())
            .expect("original random artifact must be durable before intent admission");
        assert!(journal.entries.is_empty());
        assert!(journal.lookups.is_empty());
        drop(journal);
        let mut journal = open();
        assert!(journal.file_preparation(reference.id) == Some(&payload));
        journal.put_file_preparation(payload.clone()).unwrap();
        journal.test_file_preparation_pressure(0, true).unwrap();
        // Repeated durable preparation is idempotent even under current pressure.
        journal.put_file_preparation(payload.clone()).unwrap();
        let fresh = self::reference(&node, group, &payload.logical);
        let mut another = payload.clone();
        let old_descriptor = another.descriptor.as_ref().unwrap();
        another.descriptor = Some(GroupFileDescriptor::new(
            &node,
            fresh.id,
            group,
            fresh.digest,
            old_descriptor.manifest.clone(),
            old_descriptor.asset_ciphertext_digest,
        ));
        assert!(
            journal.put_file_preparation(another).is_err(),
            "capacity rejection happens before a durable row is appended"
        );
        let mut replacement = payload.clone();
        let original = replacement.descriptor.as_ref().unwrap();
        replacement.descriptor = Some(GroupFileDescriptor::new(
            &node,
            reference.id,
            group,
            reference.digest,
            original.manifest.clone(),
            [42; 32],
        ));
        assert!(journal.put_file_preparation(replacement).is_err());
        assert!(journal.file_preparation(reference.id) == Some(&payload));
        drop(journal);
        assert!(open().file_preparation(reference.id) == Some(&payload));
    }
    fn assert_original_descriptor_cannot_be_replaced_in_body(lookup: bool) {
        let (dir, node, account, group, logical, manifest, chunk) = fixture();
        let reference = reference(&node, group, &logical);
        let original = GroupFilePayload {
            logical: logical.clone(),
            descriptor: Some(GroupFileDescriptor::new(
                &node,
                reference.id,
                group,
                reference.digest,
                manifest.clone(),
                group_file_asset_digest([chunk.as_slice()]),
            )),
        };
        let mut changed_manifest = FileManifestV3::decode(&manifest).unwrap();
        changed_manifest.v2.file_key = [41; 32];
        changed_manifest.v2.file_nonce = [42; 8];
        let replacement = GroupFilePayload {
            logical,
            descriptor: Some(GroupFileDescriptor::new(
                &node,
                reference.id,
                group,
                reference.digest,
                changed_manifest.encode(),
                [43; 32],
            )),
        };
        assert!(
            replacement
                .descriptor
                .as_ref()
                .unwrap()
                .valid_for(&reference, &replacement.logical)
        );
        let original_body = AccountGroupOperation::Send {
            message: original.prepared().unwrap(),
        };
        let replacement_body = AccountGroupOperation::Send {
            message: replacement.prepared().unwrap(),
        };
        assert_eq!(
            journal::operation_digest(group, &original_body),
            journal::operation_digest(group, &replacement_body)
        );
        let status = AccountGroupOperationStatus {
            id: reference.id,
            account_id: node.account_id(),
            group,
            kind: AccountGroupOperationKind::File,
            phase: AccountGroupOperationPhase::WaitingForExecutor,
            executions: vec![],
            committed_events: vec![],
            primary_event: None,
        };
        let journal_path = dir.path().join("body-pinning");
        std::fs::create_dir(&journal_path).unwrap();
        let open = || {
            journal::OperationJournal::open(
                &journal_path,
                "pw",
                account.public().ed25519_pub,
                node.identity.public(),
            )
            .unwrap()
        };
        let mut journal = open();
        journal.put_file_preparation(original.clone()).unwrap();
        if lookup {
            let mut row = journal::Lookup {
                id: reference.id,
                group,
                digest: reference.digest,
                body: original_body,
                status,
                cancel: false,
                sequence: 0,
            };
            journal.put_lookup(row.clone()).unwrap();
            row.body = replacement_body;
            assert!(
                journal.put_lookup(row).is_err(),
                "a lookup body must not replace the original randomized file descriptor"
            );
        } else {
            let mut row = journal::Entry {
                reference,
                body: Some(original_body),
                status,
                source: false,
                file_copy: None,
                cancel: false,
                sequence: 0,
            };
            journal.put(row.clone()).unwrap();
            row.body = Some(replacement_body);
            assert!(
                journal.put(row).is_err(),
                "an intent body must not replace the original randomized file descriptor"
            );
        }
        let id = original.descriptor.as_ref().unwrap().id;
        assert!(journal.file_preparation(id) == Some(&original));
        journal
            .test_file_preparation_pressure(usize::MAX, true)
            .unwrap();
        drop(journal);
        let journal = open();
        assert!(journal.file_preparation(id) == Some(&original));
        let body = if lookup {
            Some(&journal.lookups[&id].body)
        } else {
            journal.entries[&id].body.as_ref()
        };
        let Some(AccountGroupOperation::Send { message }) = body else {
            panic!("original file body must survive compaction");
        };
        assert!(GroupFilePayload::decode(message.encoded_body()).as_ref() == Some(&original));
    }

    #[test]
    fn file_entry_body_cannot_replace_the_retained_original_descriptor() {
        assert_original_descriptor_cannot_be_replaced_in_body(false);
    }

    #[test]
    fn file_lookup_body_cannot_replace_the_retained_original_descriptor() {
        assert_original_descriptor_cannot_be_replaced_in_body(true);
    }

    fn local_file_copy_fixture() -> (
        tempfile::TempDir,
        Arc<Node>,
        Account,
        journal::Entry,
        GroupFilePayload,
    ) {
        let (dir, node, account, group, logical, manifest, chunk) = fixture();
        let reference = reference(&node, group, &logical);
        let payload = GroupFilePayload {
            logical,
            descriptor: Some(GroupFileDescriptor::new(
                &node,
                reference.id,
                group,
                reference.digest,
                manifest,
                group_file_asset_digest([chunk.as_slice()]),
            )),
        };
        let event = crate::eventlog::EventId::new([91; 32]);
        let status = AccountGroupOperationStatus {
            id: reference.id,
            account_id: node.account_id(),
            group,
            kind: AccountGroupOperationKind::File,
            phase: AccountGroupOperationPhase::Committed,
            executions: vec![],
            committed_events: vec![event],
            primary_event: Some(event),
        };
        let entry = journal::Entry {
            reference,
            body: None,
            status,
            file_copy: None,
            source: false,
            cancel: false,
            sequence: 0,
        };
        (dir, node, account, entry, payload)
    }

    #[test]
    fn local_file_copy_descriptor_survives_compaction_restart_and_completion() {
        let (dir, node, account, mut entry, payload) = local_file_copy_fixture();
        let descriptor = payload.descriptor.unwrap();
        let path = dir.path().join("file-copy");
        std::fs::create_dir(&path).unwrap();
        let open = || {
            journal::OperationJournal::open(
                &path,
                "pw",
                account.public().ed25519_pub,
                node.identity.public(),
            )
            .unwrap()
        };
        entry.file_copy = Some(journal::GroupFileCopy {
            descriptor: descriptor.clone(),
            complete: false,
        });
        let mut journal = open();
        journal.put(entry.clone()).unwrap();
        journal
            .test_file_preparation_pressure(usize::MAX, true)
            .unwrap();
        drop(journal);
        let mut journal = open();
        assert!(journal.entries[&entry.reference.id].file_copy == entry.file_copy);
        journal
            .complete_file_copy(&entry.reference, &descriptor)
            .expect("verified result fetch completion must be durable in the original journal");
        drop(journal);
        let journal = open();
        assert!(
            journal.entries[&entry.reference.id]
                .file_copy
                .as_ref()
                .unwrap()
                .complete
        );
        assert!(journal.entries[&entry.reference.id].body.is_none());
        assert!(
            serde_json::to_vec(&journal.entries[&entry.reference.id].status).unwrap()
                == serde_json::to_vec(&entry.status).unwrap()
        );
    }

    #[test]
    fn local_file_copy_cannot_replace_its_original_descriptor() {
        let (_dir, node, _, mut entry, payload) = local_file_copy_fixture();
        let descriptor = payload.descriptor.unwrap();
        let mut journal = node.group_operations.lock().unwrap();
        entry.file_copy = Some(journal::GroupFileCopy {
            descriptor: descriptor.clone(),
            complete: false,
        });
        journal.put(entry.clone()).unwrap();
        entry.file_copy = Some(journal::GroupFileCopy {
            descriptor: GroupFileDescriptor::new(
                &node,
                descriptor.id,
                descriptor.group,
                descriptor.logical_digest,
                descriptor.manifest,
                [94; 32],
            ),
            complete: false,
        });
        assert!(
            journal.put(entry).is_err(),
            "local result metadata must pin the original random artifact"
        );
    }

    #[test]
    fn local_file_copy_completion_is_monotonic_and_only_for_committed_files() {
        let (_dir, node, _, mut entry, payload) = local_file_copy_fixture();
        let descriptor = payload.descriptor.unwrap();
        let mut journal = node.group_operations.lock().unwrap();
        entry.file_copy = Some(journal::GroupFileCopy {
            descriptor: descriptor.clone(),
            complete: true,
        });
        journal.put(entry.clone()).unwrap();
        entry.file_copy.as_mut().unwrap().complete = false;
        assert!(
            journal.put(entry.clone()).is_err(),
            "local completion cannot be rolled back after save reclamation"
        );
        entry.file_copy = None;
        assert!(
            journal.put(entry).is_err(),
            "retained completion cannot be erased"
        );
        drop(journal);
        let (_dir, node, _, mut entry, payload) = local_file_copy_fixture();
        entry.status.phase = AccountGroupOperationPhase::WaitingForExecutor;
        entry.status.primary_event = None;
        entry.status.committed_events.clear();
        entry.body = payload.logical_operation();
        entry.file_copy = Some(journal::GroupFileCopy {
            descriptor: payload.descriptor.unwrap(),
            complete: false,
        });
        assert!(
            node.group_operations.lock().unwrap().put(entry).is_err(),
            "an incomplete intent cannot claim a committed own-result copy"
        );
    }

    #[test]
    fn old_local_file_rows_keep_their_bytes_without_copy_metadata() {
        let (dir, node, account, entry, _) = local_file_copy_fixture();
        let encoded = serde_json::to_vec(&entry).unwrap();
        assert!(
            serde_json::from_slice::<serde_json::Value>(&encoded)
                .unwrap()
                .get("file_copy")
                .is_none()
        );
        let decoded: journal::Entry = serde_json::from_slice(&encoded).unwrap();
        assert!(decoded.file_copy.is_none());
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), encoded);
        let path = dir.path().join("old-file-copy");
        std::fs::create_dir(&path).unwrap();
        let mut journal = journal::OperationJournal::open(
            &path,
            "pw",
            account.public().ed25519_pub,
            node.identity.public(),
        )
        .unwrap();
        journal.put(decoded).unwrap();
        drop(journal);
        let journal = journal::OperationJournal::open(
            &path,
            "pw",
            account.public().ed25519_pub,
            node.identity.public(),
        )
        .unwrap();
        assert!(journal.entries[&entry.reference.id].file_copy.is_none());
    }

    #[tokio::test]
    async fn pending_original_file_preparation_survives_verified_local_reclamation() {
        let (dir, node, account, group, _, _, _) = fixture();
        let path = dir.path().join("pinned.bin");
        let bytes = b"explicit pending original must survive local reclamation";
        std::fs::write(&path, bytes).unwrap();
        let id = node
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        let payload = node
            .prepare_account_group_file(
                &account.account_id(),
                id,
                group,
                &path,
                FileKind::File,
                None,
                None,
                |_| {},
            )
            .await
            .unwrap();
        let manifest =
            crate::node::files::validated_manifest(&payload.descriptor.as_ref().unwrap().manifest)
                .unwrap();
        let file = manifest.file_conv();
        let before = node.log.lock().unwrap().fingerprint_and_len(&file);
        let chunk = node.log.lock().unwrap().events(&file)[0].ciphertext.clone();
        assert_eq!(
            crate::file::open_chunk_for(&manifest, 0, &chunk).unwrap(),
            bytes
        );
        assert!(
            !node.commit_file_completion(file, &[]),
            "verified local reclamation must retain an original artifact needed by a live preparation"
        );
        assert_eq!(node.log.lock().unwrap().fingerprint_and_len(&file), before);
        assert!(
            node.prepare_account_group_file(
                &account.account_id(),
                id,
                group,
                &path,
                FileKind::File,
                None,
                None,
                |_| {}
            )
            .await
            .unwrap()
                == payload
        );
    }

    #[test]
    fn bounded_file_preparation_uses_existing_compact_manifest_without_changing_crypto() {
        let (_dir, node, _, group, _, manifest, _) = fixture();
        let mut large = FileManifestV3::decode(&manifest).unwrap();
        large.v2.chunk_count = 1910;
        large.v2.size = u64::from(large.v2.chunk_count) * CHUNK_SIZE as u64;
        large.v2.chunk_hashes = vec![[7; 32]; large.v2.chunk_count as usize];
        let logical = GroupFileLogical::from_manifest(&large.encode()).unwrap();
        let reference = reference(&node, group, &logical);
        let payload = GroupFilePayload {
            logical,
            descriptor: Some(GroupFileDescriptor::new(
                &node,
                reference.id,
                group,
                reference.digest,
                large.encode(),
                [13; 32],
            )),
        };
        assert!(
            payload.prepared().is_none(),
            "fixture must exceed the complete preparation bound"
        );
        let compact = payload.compact_for_bound(&node).unwrap();
        assert!(
            compact.prepared().is_some(),
            "an ordinary frame-sized manifest must fit the signed preparation through existing MFM4"
        );
        let descriptor = compact.descriptor.as_ref().unwrap();
        let parsed = crate::file::FileManifestV4::decode(&descriptor.manifest).unwrap();
        assert_eq!(parsed.v3.v2.file_key, large.v2.file_key);
        assert_eq!(parsed.v3.v2.file_nonce, large.v2.file_nonce);
        assert_eq!(parsed.v3.v2.file_conv, large.v2.file_conv);
        assert_eq!(parsed.v3.v2.checksum, large.v2.checksum);
        assert!(descriptor.valid_for(&reference, &compact.logical));
        assert_eq!(descriptor.asset_ciphertext_digest, [13; 32]);
    }

    #[test]
    fn existing_blocking_file_stager_stops_allocating_after_runtime_close() {
        let (dir, node, _account, _group, _logical, _manifest, _chunk) = fixture();
        let path = dir.path().join("retired-stage.bin");
        std::fs::write(&path, vec![0x25; crate::file::CHUNK_SIZE * 3]).unwrap();
        let before = node.log.lock().unwrap().conversations();
        let mut progress = 0u32;
        let result = node.stage_file(&path, FileKind::File, |_| {
            progress += 1;
            node.runtime_work.close();
        });
        assert!(
            result.is_err(),
            "the actual blocking writer must observe participant retirement before allocating chunk2"
        );
        assert_eq!(progress, 1);
        let log = node.log.lock().unwrap();
        let staged: Vec<_> = log
            .conversations()
            .into_iter()
            .filter(|conv| !before.contains(conv))
            .collect();
        assert_eq!(staged.len(), 1);
        assert_eq!(log.conversation_len(&staged[0]), 1);
    }
    #[test]
    fn file_result_descriptor_binds_the_original_request_and_old_results_keep_their_bytes() {
        let (_dir, node, _account, group, logical, manifest, chunk) = fixture();
        let reference = reference(&node, group, &logical);
        let descriptor = GroupFileDescriptor::new(
            &node,
            reference.id,
            group,
            reference.digest,
            manifest,
            group_file_asset_digest([chunk.as_slice()]),
        );
        let event = Event::new(
            &node.identity,
            group,
            2,
            node.log.lock().unwrap().heads(&group),
            2,
            2,
            EventKind::FileManifest,
            b"signed-ciphertext-fixture".to_vec(),
        );
        let mut result = wire::ResultProjection {
            reference: reference.clone(),
            status: AccountGroupOperationStatus {
                id: reference.id,
                account_id: node.account_id(),
                group,
                kind: AccountGroupOperationKind::File,
                phase: AccountGroupOperationPhase::Committed,
                executions: vec![],
                committed_events: vec![event.id],
                primary_event: Some(event.id),
            },
            message_event: Some(event),
            file_descriptor: Some(descriptor.clone()),
            recall_mark: None,
        };
        assert!(result.valid_message_event());
        result.file_descriptor.as_mut().unwrap().logical_digest[0] ^= 1;
        assert!(!result.valid_message_event());
        result.file_descriptor = Some(descriptor.clone());
        result.file_descriptor.as_mut().unwrap().id = node
            .new_account_group_operation_id(&node.account_id())
            .unwrap();
        assert!(!result.valid_message_event());
        result.file_descriptor = Some(descriptor.clone());
        result.file_descriptor.as_mut().unwrap().manifest[8] ^= 1;
        assert!(!result.valid_message_event());
        result.file_descriptor = Some(descriptor);
        result.file_descriptor.as_mut().unwrap().creator.x25519_pub[0] ^= 1;
        assert!(!result.valid_message_event());
        result.file_descriptor = None;
        assert!(
            !result.valid_message_event(),
            "a FileManifest copy needs the original prepared descriptor"
        );
        result.message_event = None;
        #[derive(Serialize)]
        struct LegacyResult<'a> {
            reference: &'a journal::OperationReference,
            status: &'a AccountGroupOperationStatus,
            #[serde(skip_serializing_if = "Option::is_none")]
            message_event: Option<&'a Event>,
        }
        assert_eq!(
            serde_json::to_vec(&result).unwrap(),
            serde_json::to_vec(&LegacyResult {
                reference: &result.reference,
                status: &result.status,
                message_event: None
            })
            .unwrap()
        );
    }
    #[test]
    fn logical_file_digest_does_not_change_when_original_random_preparation_is_attached() {
        let (_dir, node, _account, group, logical, manifest, chunk) = fixture();
        let reference = reference(&node, group, &logical);
        let descriptor = GroupFileDescriptor::new(
            &node,
            reference.id,
            group,
            reference.digest,
            manifest,
            group_file_asset_digest([chunk.as_slice()]),
        );
        let with_preparation = AccountGroupOperation::Send {
            message: GroupFilePayload {
                logical,
                descriptor: Some(descriptor),
            }
            .prepared()
            .unwrap(),
        };
        assert_eq!(
            journal::operation_digest(group, &with_preparation),
            reference.digest,
            "the original logical ID binds content metadata independently of random encryption parameters"
        );
    }
    #[test]
    fn original_file_descriptor_rejects_relay_substitution_and_manifest_tampering() {
        let (dir, node, account, group, logical, manifest, chunk) = fixture();
        let reference = reference(&node, group, &logical);
        let descriptor = GroupFileDescriptor::new(
            &node,
            reference.id,
            group,
            reference.digest,
            manifest,
            group_file_asset_digest([chunk.as_slice()]),
        );
        assert!(descriptor.valid_for(&reference, &logical));
        let mut changed = descriptor.clone();
        changed.manifest[8] ^= 1;
        assert!(!changed.valid_for(&reference, &logical));
        let mut changed = descriptor.clone();
        changed.asset_ciphertext_digest[0] ^= 1;
        assert!(!changed.valid_for(&reference, &logical));
        let (dm, _) = mpsc::unbounded_channel();
        let (channels, _) = mpsc::unbounded_channel();
        let (files, _) = mpsc::unbounded_channel();
        let sibling = Node::open_with_account(
            DeviceIdentity::generate(),
            Account::from_secret_bytes(account.secret_bytes()),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channels,
            files,
            &dir.path().join("sibling-events"),
            &dir.path().join("sibling-sent"),
            "pw",
        )
        .unwrap();
        let substitute = GroupFileDescriptor::new(
            &sibling,
            reference.id,
            group,
            reference.digest,
            descriptor.manifest.clone(),
            descriptor.asset_ciphertext_digest,
        );
        assert!(
            !substitute.valid_for(&reference, &logical),
            "a current sibling cannot replace the original creator's prepared asset signature"
        );
        let encoded = GroupFilePayload {
            logical,
            descriptor: Some(descriptor),
        }
        .encode()
        .unwrap();
        assert!(GroupFilePayload::decode(&encoded).is_some());
        let mut trailing = encoded;
        trailing.push(0);
        assert!(GroupFilePayload::decode(&trailing).is_none());
    }
    #[test]
    fn existing_non_file_operation_digest_bytes_remain_unchanged() {
        let group = ConversationId::new([0x51; 32]);
        let text = PreparedAccountGroupMessage::from_plaintext(
            AccountGroupOperationKind::Text,
            crate::node::MessageBody::new(b"existing text".to_vec(), None).encode(),
        )
        .unwrap();
        let sticker = PreparedAccountGroupMessage::from_plaintext(
            AccountGroupOperationKind::Sticker,
            crate::node::MessageBody::sticker("1f602".into(), "😂".as_bytes().to_vec()).encode(),
        )
        .unwrap();
        for operation in [
            AccountGroupOperation::Send { message: text },
            AccountGroupOperation::Send { message: sticker },
            AccountGroupOperation::Rename {
                name: "unchanged".into(),
            },
            AccountGroupOperation::SetHistoryPolicy {
                policy: crate::channel::HistoryPolicy::LastTen,
            },
            AccountGroupOperation::Dissolve,
            AccountGroupOperation::RemoveAccount {
                target_account_id: "11".repeat(16),
            },
            AccountGroupOperation::TransferOwnership {
                target_account_id: "22".repeat(16),
            },
        ] {
            let mut old = Sha256::new();
            old.update(b"mesh-talk-account-group-operation-body-v1");
            old.update(group.as_bytes());
            old.update(serde_json::to_vec(&operation).unwrap());
            let old: [u8; 32] = old.finalize().into();
            assert_eq!(journal::operation_digest(group, &operation), old);
        }
    }
}
