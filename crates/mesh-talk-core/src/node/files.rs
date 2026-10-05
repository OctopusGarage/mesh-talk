//! File transfer: chunked send (DM/account/channel) and save, streamed to/from disk
//! so neither side ever buffers the whole file. Files become a v2 (`MFM2`) manifest
//! plus one chunk event per `CHUNK_SIZE` piece in a dedicated per-file conversation;
//! the chunk events ride the normal bounded sync, which already re-requests only the
//! events a peer is missing — so transfer RESUMES across reconnects for free.

use super::node::MAX_FILE_SIZE;
use super::*;

pub(in crate::node) fn same_file_transfer(
    left: &crate::file::AnyManifest,
    right: &crate::file::AnyManifest,
) -> bool {
    use crate::file::AnyManifest;
    fn v1(mut manifest: crate::file::FileManifest) -> crate::file::FileManifest {
        manifest.name.clear();
        manifest.mime.clear();
        manifest
    }
    fn v2(mut manifest: crate::file::FileManifestV2) -> crate::file::FileManifestV2 {
        manifest.name.clear();
        manifest.mime.clear();
        manifest
    }
    match (left, right) {
        (AnyManifest::V1(left), AnyManifest::V1(right)) => v1(left.clone()) == v1(right.clone()),
        (AnyManifest::V2(left), AnyManifest::V2(right)) => v2(left.clone()) == v2(right.clone()),
        (AnyManifest::V3(left), AnyManifest::V3(right)) => {
            v2(left.v2.clone()) == v2(right.v2.clone())
        }
        _ => false,
    }
}
use crate::eventlog::event::{Author, ConversationId, EventKind};
use crate::file::{
    chunk_count_for, chunk_hash, file_checksum, generate_file_nonce, open_chunk_for,
    reassemble_and_verify, seal_chunk_indexed, AnyManifest, FileKey, FileKind, FileManifestV2,
    FileManifestV3, CHUNK_SIZE,
};
use crate::node::conversation::dm_conversation_id;
use sha2::{Digest, Sha256};
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::sync::Arc;

pub(in crate::node) fn validated_manifest(bytes: &[u8]) -> Option<AnyManifest> {
    use bincode::Options;
    if bytes.len() > crate::transport::MAX_PLAINTEXT {
        return None;
    }
    let decode = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(crate::transport::MAX_PLAINTEXT as u64)
        .reject_trailing_bytes();
    let manifest = if let Some(body) = bytes.strip_prefix(b"MFM3") {
        AnyManifest::V3(decode.deserialize::<FileManifestV3>(body).ok()?)
    } else if let Some(body) = bytes.strip_prefix(b"MFM2") {
        AnyManifest::V2(decode.deserialize::<FileManifestV2>(body).ok()?)
    } else {
        AnyManifest::V1(
            decode
                .deserialize::<crate::file::FileManifest>(bytes)
                .ok()?,
        )
    };
    if manifest.size() > MAX_FILE_SIZE || manifest.chunk_count() == 0 {
        return None;
    }
    match &manifest {
        AnyManifest::V1(m) if m.chunk_count != chunk_count_for(m.size) => return None,
        AnyManifest::V2(m) | AnyManifest::V3(FileManifestV3 { v2: m, .. })
            if m.chunk_size == 0
                || m.chunk_size as usize > CHUNK_SIZE
                || u64::from(m.chunk_count) != m.size.div_ceil(u64::from(m.chunk_size)).max(1)
                || m.chunk_hashes.len() != m.chunk_count as usize =>
        {
            return None
        }
        _ => {}
    }
    Some(manifest)
}

/// A durable plaintext row is a local metadata cache, not authority without its
/// retained signed original. Host conversations may legitimately differ from
/// the original device-pair conversation; neither may alias the chunk stream.
pub(in crate::node) fn eligible_manifest_row(
    entry: &ReceivedEntry,
    original: Option<&crate::eventlog::Event>,
    existing: Option<&AnyManifest>,
) -> Option<AnyManifest> {
    let original = original?;
    let manifest = validated_manifest(&entry.plaintext)?;
    (original.id == entry.event_id
        && original.kind == EventKind::FileManifest
        && original.verify_integrity()
        && original.verify_signature()
        && entry.from == original.author.user_id()
        && entry.wall_clock == original.wall_clock
        && manifest.file_conv() != original.conversation_id
        && manifest.file_conv() != entry.conversation
        && existing.is_none_or(|existing| same_file_transfer(existing, &manifest)))
    .then_some(manifest)
}

/// Progress of a file transfer: `done`/`total` chunks. The terminal callback always
/// has `done == total`.
#[derive(Debug, Clone, Copy)]
pub struct FileProgress {
    pub done: u32,
    pub total: u32,
}

impl Node {
    pub(in crate::node) fn reseed_live_file_book(
        &self,
        delivery: &super::delivery_store::DeliveryStore,
    ) {
        let mut book = FileBook::new();
        let mut rows = Vec::new();
        {
            let records = self.received_files.lock().expect("files lock not poisoned");
            let log = self.log.lock().expect("log lock not poisoned");
            for conversation in records.conversations() {
                for entry in records.entries(&conversation) {
                    if delivery.file_erased(entry.conversation, entry.event_id)
                        || delivery.manifest_event_erased(entry.event_id)
                    {
                        continue;
                    }
                    let original = log.get(&entry.event_id);
                    let decoded = validated_manifest(&entry.plaintext);
                    let existing = decoded.as_ref().and_then(|m| book.manifest(&m.file_conv()));
                    if let Some(manifest) = eligible_manifest_row(&entry, original, existing) {
                        rows.push((manifest.file_conv(), entry.event_id));
                        book.record_event(entry.event_id, manifest);
                        book.mark_emitted(entry.event_id);
                    }
                }
            }
        }
        let live = book
            .file_convs()
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let legacy = self
            .privacy
            .state
            .read()
            .expect("privacy lock not poisoned")
            .as_ref()
            .map(|state| {
                state
                    .file_scopes
                    .iter()
                    .flat_map(|(file, scopes)| {
                        scopes
                            .iter()
                            .filter(|scope| state.legacy_scope_ids.contains(&scope.event))
                            .map(|scope| (*file, scope.clone()))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut scopes: std::collections::HashMap<_, Vec<_>> = Default::default();
        {
            let log = self.log.lock().expect("log lock not poisoned");
            let mut own_files = std::collections::HashSet::new();
            for (file, id) in rows {
                if let Some(event) = log.get(&id).filter(|event| {
                    event.kind == EventKind::FileManifest
                        && event.verify_integrity()
                        && event.verify_signature()
                }) {
                    if event.author.ed25519_pub() == &self.identity.public().ed25519_pub {
                        own_files.insert(file);
                    }
                    scopes
                        .entry(file)
                        .or_default()
                        .push(super::privacy_runtime::ManifestScope {
                            parent: event.conversation_id,
                            author: event.author,
                            event: id,
                        });
                }
            }
            for (file, scope) in legacy {
                if own_files.contains(&file)
                    && !delivery.manifest_event_erased(scope.event)
                    && log.get(&scope.event).is_some_and(|event| {
                        event.kind == EventKind::FileManifest
                            && event.author == scope.author
                            && event.conversation_id == scope.parent
                            && event.author.ed25519_pub() == &self.identity.public().ed25519_pub
                            && event.verify_integrity()
                            && event.verify_signature()
                    })
                {
                    let retained = scopes.entry(file).or_default();
                    if !retained.contains(&scope) {
                        retained.push(scope);
                    }
                }
            }
        }
        *self.files.lock().expect("files lock not poisoned") = book;
        self.pending_files
            .lock()
            .expect("pending files lock not poisoned")
            .retain(|file| live.contains(file));
        if let Some(state) = self
            .privacy
            .state
            .write()
            .expect("privacy lock not poisoned")
            .as_mut()
        {
            state.file_scopes = scopes;
        }
        self.install_file_card_scopes(delivery);
    }
    /// Send the file at `path` to a DM peer. Streams + seals it into a fresh per-file
    /// conversation, seals the manifest to the recipient, posts a `FileManifest`
    /// event into the DM conversation, and distributes both. Returns the per-file
    /// conversation id (the handle for the recipient to save).
    pub async fn send_file_dm(
        self: &Arc<Self>,
        recipient: &str,
        path: &Path,
        kind: FileKind,
    ) -> Result<ConversationId, NodeError> {
        self.send_file_dm_progress(recipient, path, kind, |_| {})
            .await
    }

    /// [`Node::send_file_dm`] with a progress callback invoked as chunks are sealed.
    pub async fn send_file_dm_progress(
        self: &Arc<Self>,
        recipient: &str,
        path: &Path,
        kind: FileKind,
        on_progress: impl FnMut(FileProgress) + Send + 'static,
    ) -> Result<ConversationId, NodeError> {
        let (id, file) = self
            .enqueue_file_dm_progress(recipient, path, kind, on_progress)
            .await?;
        self.flush_delivery(id).await;
        self.flush_file_delivery(id).await;
        Ok(file)
    }

    /// Durably accept one immutable file card. Network retries belong to the
    /// accept-loop worker; the card ID remains stable through local recovery.
    pub async fn enqueue_file_dm(
        self: &Arc<Self>,
        recipient: &str,
        path: &Path,
        kind: FileKind,
    ) -> Result<(crate::eventlog::EventId, ConversationId), NodeError> {
        self.enqueue_file_dm_progress(recipient, path, kind, |_| {})
            .await
    }

    pub async fn enqueue_file_dm_progress(
        self: &Arc<Self>,
        recipient: &str,
        path: &Path,
        kind: FileKind,
        on_progress: impl FnMut(FileProgress) + Send + 'static,
    ) -> Result<(crate::eventlog::EventId, ConversationId), NodeError> {
        let peer = self
            .routing_peer(recipient)
            .ok_or_else(|| NodeError::UnknownPeer(recipient.to_string()))?;

        self.initiate_device_locally(&peer.public)
            .await
            .map_err(|e| NodeError::Log(crate::eventlog::LogError::Io(e)))?;
        let (manifest, file_conv) = self.stage_file_blocking(path, kind, on_progress).await?;
        let dm_conv = dm_conversation_id(&self.identity.public(), &peer.public);
        let id = self.accept_staged_manifest(
            std::slice::from_ref(&peer),
            dm_conv,
            peer.account_id.clone(),
            &manifest,
        )?;
        Ok((id, file_conv))
    }

    /// Send a file to an ACCOUNT: stage it once (shared symmetric chunks), then seal
    /// the manifest to every known device of `target_account_id` AND our own other
    /// devices (self-sync), delivering the file + manifest to each.
    pub async fn send_file_to_account(
        self: &Arc<Self>,
        target_account_id: &str,
        path: &Path,
        kind: FileKind,
    ) -> Result<ConversationId, NodeError> {
        self.send_file_to_account_progress(target_account_id, path, kind, |_| {})
            .await
    }

    /// [`Node::send_file_to_account`] with a progress callback.
    pub async fn send_file_to_account_progress(
        self: &Arc<Self>,
        target_account_id: &str,
        path: &Path,
        kind: FileKind,
        on_progress: impl FnMut(FileProgress) + Send + 'static,
    ) -> Result<ConversationId, NodeError> {
        let (id, file) = self
            .enqueue_file_to_account_progress(target_account_id, path, kind, on_progress)
            .await?;
        self.flush_delivery(id).await;
        self.flush_file_delivery(id).await;
        Ok(file)
    }

    pub async fn enqueue_file_to_account(
        self: &Arc<Self>,
        target_account_id: &str,
        path: &Path,
        kind: FileKind,
    ) -> Result<(crate::eventlog::EventId, ConversationId), NodeError> {
        self.enqueue_file_to_account_progress(target_account_id, path, kind, |_| {})
            .await
    }

    pub async fn enqueue_file_to_account_progress(
        self: &Arc<Self>,
        target_account_id: &str,
        path: &Path,
        kind: FileKind,
        on_progress: impl FnMut(FileProgress) + Send + 'static,
    ) -> Result<(crate::eventlog::EventId, ConversationId), NodeError> {
        self.enqueue_file_to_account_progress_if(
            target_account_id,
            path,
            kind,
            on_progress,
            |accept| accept(),
        )
        .await
    }

    /// Calls `authorize` initially, around the synchronous local privacy grant,
    /// and at final WAL acceptance after staging. For each successful invocation,
    /// execute the supplied synchronous operation exactly once and propagate its result.
    /// The final WAL operation is single-use and must run under the owner guard.
    pub async fn enqueue_file_to_account_progress_if(
        self: &Arc<Self>,
        target_account_id: &str,
        path: &Path,
        kind: FileKind,
        on_progress: impl FnMut(FileProgress) + Send + 'static,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(crate::eventlog::EventId, ConversationId), NodeError> {
        authorize(&mut || Ok(()))?;
        let dests = self.account_fanout_targets(target_account_id);
        if !dests
            .iter()
            .any(|p| p.account_id.as_deref() == Some(target_account_id))
        {
            return Err(NodeError::UnknownPeer(target_account_id.to_string()));
        }
        self.initiate_contact_locally_if(target_account_id, &mut authorize)
            .await?;

        let (manifest, file_conv) = self.stage_file_blocking(path, kind, on_progress).await?;
        // Record the outgoing file ONCE under the account conversation (the UI's host
        // conversation for this contact), so it shows as our own message in account
        // history. The per-device manifest events below each get a distinct event id; we
        // record the first that lands (its id keys the durable store) rather than once per
        // device — which would duplicate the bubble.
        let account_conv = super::conversation::account_conversation_id(
            &self.account.account_id(),
            target_account_id,
        );
        let id = self.accept_staged_manifest_if(
            &dests,
            account_conv,
            Some(target_account_id.to_owned()),
            &manifest,
            &mut authorize,
        )?;
        Ok((id, file_conv))
    }

    pub(in crate::node) fn accept_staged_manifest(
        &self,
        peers: &[crate::discovery::PeerRecord],
        host: ConversationId,
        target_account: Option<String>,
        manifest: &FileManifestV3,
    ) -> Result<crate::eventlog::EventId, NodeError> {
        self.accept_staged_manifest_if(peers, host, target_account, manifest, &mut |accept| {
            accept()
        })
    }

    fn accept_staged_manifest_if(
        &self,
        peers: &[crate::discovery::PeerRecord],
        host: ConversationId,
        target_account: Option<String>,
        manifest: &FileManifestV3,
        authorize: &mut impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<crate::eventlog::EventId, NodeError> {
        use super::delivery_store::{DeliveryDestination, DeliveryTransaction, OutgoingDelivery};
        let mut store = self.delivery.lock().expect("delivery lock not poisoned");
        self.recover_delivery(&mut store).map_err(NodeError::Log)?;
        let own = self.identity.public();
        let author = Author::from_ed25519(own.ed25519_pub);
        let clock = super::node::now_millis();
        let plaintext = manifest.encode();
        let mut destinations = Vec::with_capacity(peers.len());
        {
            let log = self.log.lock().expect("log lock not poisoned");
            for peer in peers {
                let conv = dm_conversation_id(&own, &peer.public);
                let (parents, lamport) = log.prepare(&conv);
                let seq = log.version_vector(&conv).get(&author).copied().unwrap_or(0) + 1;
                let wire = crate::dm::seal(&self.identity, &peer.public.x25519_pub, &plaintext)
                    .map_err(NodeError::Seal)?;
                let event = crate::eventlog::Event::new(
                    &self.identity,
                    conv,
                    seq,
                    parents,
                    lamport,
                    clock,
                    EventKind::FileManifest,
                    wire,
                );
                if !super::session::event_fits_frame(&event) {
                    return Err(NodeError::InvalidInput(
                        "file manifest exceeds transport frame".into(),
                    ));
                }
                destinations.push(DeliveryDestination {
                    device: peer.public.clone(),
                    account: peer.account_id.clone(),
                    event,
                    receipt_eligible: peer.account_id.is_some()
                        && peer.account_id == target_account
                        && peer.account_id.as_deref() != Some(self.account_id().as_str()),
                });
            }
            log.sync().map_err(NodeError::Log)?;
        }
        let id = destinations
            .first()
            .ok_or_else(|| NodeError::File("no file destinations".into()))?
            .event
            .id;
        let final_chunk = self
            .log
            .lock()
            .expect("log lock not poisoned")
            .events(&manifest.v2.file_conv)
            .last()
            .map(|event| event.id);
        let file = super::delivery_store::FileCard {
            id,
            conversation: host,
            wall_clock: clock,
            file_conversation: manifest.v2.file_conv,
            final_chunk,
            chunk_count: manifest.v2.chunk_count,
            destinations: destinations
                .iter()
                .map(|d| super::delivery_store::FileDestination {
                    binding: super::delivery_store::DeliveryReference {
                        device: d.device.clone(),
                        account: d.account.clone(),
                        event_id: d.event.id,
                        receipt_eligible: d.receipt_eligible,
                    },
                    active: true,
                })
                .collect(),
            completion_binding: None,
        };
        store
            .validate_staged_file(
                &file,
                &self.log.lock().expect("log lock not poisoned"),
                &author,
            )
            .map_err(NodeError::Log)?;
        let mut transaction = Some(DeliveryTransaction::OutgoingManifest {
            message: OutgoingDelivery {
                logical_id: id,
                sender_account: self.account_id(),
                recipient_account: target_account,
                conversation: host,
                wall_clock: clock,
                destinations,
            },
            received: Box::new(super::received_log::ReceivedEntry {
                event_id: id,
                conversation: host,
                from: own.user_id(),
                wall_clock: clock,
                plaintext,
            }),
            file,
        });
        authorize(&mut || {
            store
                .begin(transaction.take().expect("accept is single-use"))
                .map(|_| ())
                .map_err(NodeError::Log)
        })?;
        if self.recover_delivery(&mut store).is_ok() {
            let mut book = self.files.lock().expect("files lock not poisoned");
            book.mark_emitted(id);
            book.record_event(id, AnyManifest::V3(manifest.clone()));
        }
        self.delivery_notify.notify_one();
        Ok(id)
    }

    /// Send the file at `path` to a channel we hold the key for.
    pub async fn send_file_channel(
        self: &Arc<Self>,
        channel: ConversationId,
        path: &Path,
        kind: FileKind,
    ) -> Result<ConversationId, NodeError> {
        self.send_file_channel_progress(channel, path, kind, |_| {})
            .await
    }

    /// [`Node::send_file_channel`] with a progress callback.
    pub async fn send_file_channel_progress(
        self: &Arc<Self>,
        channel: ConversationId,
        path: &Path,
        kind: FileKind,
        on_progress: impl FnMut(FileProgress) + Send + 'static,
    ) -> Result<ConversationId, NodeError> {
        let (manifest, file_conv) = self.stage_file_blocking(path, kind, on_progress).await?;
        // Publish our sender-key distribution before the first seal (see
        // `send_channel_message_reply`).
        let (members, already_distributed) = {
            let mut book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state_mut(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            let epoch = state.epoch();
            (state.members().to_vec(), state.has_my_sender(epoch))
        };
        if !already_distributed {
            self.distribute_my_sender_key(channel, &members)?;
        }
        let sealed = {
            let mut book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state_mut(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            state
                .seal_sender_message(&manifest.encode())
                .map_err(|e| NodeError::File(format!("manifest sealing failed: {e}")))?
        };
        // Persist our advanced sending chain (see `send_channel_message_reply`).
        self.persist_my_senders(channel);
        let seq = self.append_event(channel, EventKind::FileManifest, sealed)?;
        self.record_sent_manifest(channel, channel, seq, &manifest)?;
        self.distribute_channel(file_conv, &members).await;
        self.distribute_channel(channel, &members).await;
        Ok(file_conv)
    }

    /// Off-reactor wrapper around [`Node::stage_file`]: staging is a synchronous burst of
    /// disk reads + per-chunk AEAD + per-chunk encrypted-log appends/flushes, so run it on
    /// the blocking pool to keep it off a tokio worker (the receive/save path is offloaded
    /// the same way). The result is identical — just executed on a blocking thread.
    async fn stage_file_blocking(
        self: &Arc<Self>,
        path: &Path,
        kind: FileKind,
        on_progress: impl FnMut(FileProgress) + Send + 'static,
    ) -> Result<(FileManifestV3, ConversationId), NodeError> {
        let node = Arc::clone(self);
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || node.stage_file(&path, kind, on_progress))
            .await
            .map_err(|e| NodeError::File(format!("stage join error: {e}")))?
    }

    /// Stream `path` from disk, sealing each `CHUNK_SIZE` piece (deterministic-nonce v2
    /// AEAD) into a fresh per-file conversation as one chunk event, hashing each chunk
    /// (and the whole file) as we go. Never holds more than one chunk in memory.
    /// Returns the (unsealed) v2 manifest; the caller seals + posts it into the conv.
    pub(in crate::node) fn stage_file(
        &self,
        path: &Path,
        kind: FileKind,
        mut on_progress: impl FnMut(FileProgress),
    ) -> Result<(FileManifestV3, ConversationId), NodeError> {
        let size = std::fs::metadata(path)
            .map_err(|e| NodeError::File(format!("stat file: {e}")))?
            .len();
        if size > MAX_FILE_SIZE {
            return Err(NodeError::InvalidInput(format!(
                "file too large: {size} bytes (max {MAX_FILE_SIZE})"
            )));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());

        // MFM3 carries one hash per chunk. Its existing positional wire format
        // must fit a sync frame, even when the declared file size is supported.
        // Estimate from an empty hash list before reading or staging any chunks.
        let prospective = FileManifestV3 {
            v2: FileManifestV2 {
                name: name.clone(),
                size,
                mime: mime_from_name(&name),
                checksum: [0; 32],
                file_key: [0; 32],
                file_nonce: [0; 8],
                file_conv: ConversationId::new([0; 32]),
                chunk_size: CHUNK_SIZE as u32,
                chunk_count: chunk_count_for(size),
                chunk_hashes: vec![],
            },
            kind,
        };
        let sealed_overhead = bincode::serialized_size(&crate::dm::SealedEnvelope {
            ephemeral_pub: [0; 32],
            ciphertext: vec![0; 16],
        })
        .map_err(|_| NodeError::File("manifest encoding failed".into()))?;
        let wire_size = (prospective.encode().len() as u64)
            .saturating_add(u64::from(prospective.v2.chunk_count) * 32)
            .saturating_add(sealed_overhead);
        if wire_size > crate::transport::MAX_PLAINTEXT as u64 {
            return Err(NodeError::InvalidInput(
                "file manifest exceeds transport frame".into(),
            ));
        }
        let prospective_event = crate::eventlog::Event::new(
            &self.identity,
            ConversationId::new([0; 32]),
            1,
            vec![],
            1,
            0,
            EventKind::FileManifest,
            vec![0; wire_size as usize],
        );
        if !super::session::event_fits_frame(&prospective_event) {
            return Err(NodeError::InvalidInput(
                "file manifest exceeds transport frame".into(),
            ));
        }

        let key = FileKey::generate();
        let file_nonce = generate_file_nonce();
        let file_conv = crate::channel::new_channel_id();
        let chunk_count = chunk_count_for(size);

        let file = std::fs::File::open(path).map_err(|e| NodeError::File(format!("open: {e}")))?;
        let mut reader = BufReader::new(file);
        let mut whole = Sha256::new();
        let mut chunk_hashes: Vec<[u8; 32]> = Vec::with_capacity(chunk_count as usize);
        let mut buf = vec![0u8; CHUNK_SIZE];

        for index in 0..chunk_count {
            let n = read_full(&mut reader, &mut buf)
                .map_err(|e| NodeError::File(format!("read: {e}")))?;
            let plain = &buf[..n];
            whole.update(plain);
            chunk_hashes.push(chunk_hash(plain));
            let sealed = seal_chunk_indexed(&key, &file_nonce, index, plain)
                .map_err(|e| NodeError::File(format!("chunk seal: {e}")))?;
            self.append_event(file_conv, EventKind::Message, sealed)?;
            on_progress(FileProgress {
                done: index + 1,
                total: chunk_count,
            });
        }

        // MEDIA vs ATTACHMENT split (storage side) — now driven by the sender's INTENT, not
        // the file extension: only a file sent via the media button is copied into the
        // durable chat-media store (for the inline preview, surviving chunk prune + restart).
        // An attachment keeps the chunks + manual-save flow even if it's an image/video.
        if kind == FileKind::Media {
            // Best-effort: a media-store write failure must not fail the send (the chunked
            // transfer is the source of truth on the wire); we just lose the durable preview.
            let _ = self.media.store_from_path(file_conv, &name, path);
        }

        let mime = mime_from_name(&name);
        let manifest = FileManifestV3 {
            v2: FileManifestV2 {
                name,
                size,
                mime,
                checksum: whole.finalize().into(),
                file_key: *key.as_bytes(),
                file_nonce,
                file_conv,
                chunk_size: CHUNK_SIZE as u32,
                chunk_count,
                chunk_hashes,
            },
            kind,
        };
        Ok((manifest, file_conv))
    }

    /// Record a manifest we just SENT into the durable file stores so the file shows as an
    /// outgoing message in OUR history (and is previewable/saveable), persisting across
    /// restart. Records into `received_files` keyed by `record_conv` (the UI's host
    /// conversation: the DM/channel conv, or the ACCOUNT conv for an account send) with
    /// `from = our own user-id` (so `conversation_files` derives `from_me`), and into the
    /// FileBook so `read_file`/`save_file` resolve the manifest (our own chunks are in the
    /// file_conv log). The manifest event's id + wall-clock are read from `event_conv` by
    /// `seq` (the value `append_event` returned) — for a DM/channel send `event_conv ==
    /// record_conv`; for an account send the event lives in a per-device DM conv while we
    /// file it under the account conv. Idempotent: the `received_files`/FileBook dedup on
    /// the event id, so the account path's repeated calls (one per device, same manifest)
    /// record it once.
    pub(in crate::node) fn record_sent_manifest(
        &self,
        record_conv: ConversationId,
        event_conv: ConversationId,
        seq: u64,
        manifest: &FileManifestV3,
    ) -> Result<(), NodeError> {
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        // Resolve the just-appended manifest event's content-addressed id + wall-clock.
        let appended = {
            let log = self.log.lock().expect("log mutex not poisoned");
            log.events(&event_conv)
                .into_iter()
                .find(|e| {
                    e.kind == EventKind::FileManifest && e.author == self_author && e.seq == seq
                })
                .map(|e| (e.id, e.wall_clock))
        };
        let Some((event_id, wall_clock)) = appended else {
            return Err(NodeError::File("sent manifest event unavailable".into()));
        };
        self.remember_sent_manifest_scope(manifest.v2.file_conv, event_conv, self_author, event_id)
            .map_err(NodeError::Log)?;
        let plaintext = manifest.encode();
        self.received_files
            .lock()
            .expect("received_files mutex not poisoned")
            .record(
                record_conv,
                self.identity.public().user_id(),
                wall_clock,
                &plaintext,
                event_id,
            )
            .map_err(NodeError::Log)?;
        let mut files = self.files.lock().expect("files mutex not poisoned");
        files.mark_emitted(event_id);
        files.record_event(event_id, AnyManifest::V3(manifest.clone()));
        Ok(())
    }

    /// Read durable chat-media bytes for `file_conv` from the media store, for inline
    /// display. Returns `None` if no media is stored (e.g. a generic attachment, or media
    /// not yet received-complete). This is the DURABLE display path — distinct from
    /// [`Node::read_file`], which reassembles the transient chunks (gone after prune).
    pub fn read_media(&self, file_conv: ConversationId) -> Option<Vec<u8>> {
        self.media.read(file_conv)
    }

    /// Whether durable media bytes for `file_conv` exist (drives the UI's store-vs-chunk
    /// fallback). True only for media files that have been received-complete (or sent).
    pub fn has_media(&self, file_conv: ConversationId) -> bool {
        self.media.contains(file_conv)
    }

    /// Receive-complete persist for MEDIA: if `file_conv` is a media file (image/video by
    /// name) we hold ALL chunks of but haven't yet copied into the durable media store,
    /// reassemble + verify it (`read_file`) and write the bytes into the store. After this
    /// the media is durable, so its transient chunks are safe to prune. A no-op for a
    /// generic attachment (never stored), an incomplete file, or one already stored.
    /// Returns true iff it newly persisted media. Best-effort — failures are swallowed and
    /// retried on the next sync tick.
    pub(in crate::node) fn persist_media_if_complete(&self, file_conv: ConversationId) -> bool {
        // Decide media-vs-attachment by the sender's INTENT (manifest kind); for legacy
        // manifests with no kind, fall back to the filename heuristic. Only media is copied
        // to the durable store; an attachment keeps the chunks + manual-save flow.
        let (name, media) = {
            let files = self.files.lock().expect("files mutex not poisoned");
            let Some(m) = files.manifest(&file_conv) else {
                return false;
            };
            (
                m.name().to_string(),
                crate::node::media_store::manifest_is_media(m),
            )
        };
        if !media {
            return false; // attachment — never written to the media store
        }
        if self.media.contains(file_conv) && self.has_verified_file_completion(file_conv) {
            return false; // already durable
        }
        let candidates = self.completion_candidates(file_conv);
        // Need all chunks present; `read_file` enforces completeness + verifies integrity.
        match self.read_file(file_conv) {
            Ok(bytes) => {
                if self.media.store_bytes(file_conv, &name, &bytes).is_ok() {
                    // The durable media copy now exists, so the transient chunks are
                    // reclaimable — mirror save_file's prune (history still shows the bubble
                    // from the FileBook + received_files; the preview loads from the store).
                    self.commit_file_completion(file_conv, &candidates);
                    self.pending_files
                        .lock()
                        .expect("pending_files mutex not poisoned")
                        .remove(&file_conv);
                    true
                } else {
                    false
                }
            }
            Err(_) => false, // not all chunks yet (or verify failed) — retry next tick
        }
    }

    /// How many chunks of `file_conv` we hold vs. how many the manifest expects.
    /// `None` if the manifest hasn't synced yet. Drives resume + progress in the UI.
    pub fn file_progress(&self, file_conv: ConversationId) -> Option<FileProgress> {
        let total = self
            .files
            .lock()
            .expect("files mutex not poisoned")
            .manifest(&file_conv)?
            .chunk_count();
        let have = {
            let log = self.log.lock().expect("log mutex not poisoned");
            log.events(&file_conv)
                .into_iter()
                .filter(|e| e.kind == EventKind::Message)
                .count() as u32
        };
        Some(FileProgress {
            done: have.min(total),
            total,
        })
    }

    /// Whether a save can attempt current chunks or a retained managed-media copy.
    /// Historical exports alone do not imply that this node still holds the bytes.
    /// This bounded metadata/path check does not read content: save verifies the
    /// complete managed copy's size and checksum on the blocking export path.
    pub fn file_ready_to_save(&self, file_conv: ConversationId) -> bool {
        self.file_progress(file_conv).is_some_and(|progress| {
            if progress.done == progress.total {
                return true;
            }
            let manifest = self
                .files
                .lock()
                .expect("files lock not poisoned")
                .manifest(&file_conv)
                .cloned();
            manifest.is_some_and(|manifest| {
                self.managed_media_for_export(file_conv, &manifest)
                    .is_some()
            })
        })
    }

    /// Reassemble + verify a received file into its decrypted bytes (whole, in memory
    /// — used for inline image preview). Errors if the manifest is unknown, not all
    /// chunks have synced, or verification fails. For large files prefer
    /// [`Node::save_file`], which streams to disk.
    pub fn read_file(&self, file_conv: ConversationId) -> Result<Vec<u8>, NodeError> {
        let manifest = self
            .files
            .lock()
            .expect("files mutex not poisoned")
            .manifest(&file_conv)
            .cloned()
            .ok_or_else(|| NodeError::File("unknown file".into()))?;
        let chunks: Vec<Vec<u8>> = self.collect_chunks(file_conv);
        if chunks.len() as u32 != manifest.chunk_count() {
            return Err(NodeError::File(format!(
                "file incomplete: {}/{} chunks",
                chunks.len(),
                manifest.chunk_count()
            )));
        }
        match &manifest {
            AnyManifest::V1(m) => reassemble_and_verify(m, &chunks)
                .map_err(|e| NodeError::File(format!("reassemble: {e}"))),
            AnyManifest::V2(_) | AnyManifest::V3(_) => {
                let mut out = Vec::new();
                for (i, ct) in chunks.iter().enumerate() {
                    let plain = open_chunk_for(&manifest, i as u32, ct)
                        .map_err(|e| NodeError::File(format!("open chunk {i}: {e}")))?;
                    if (out.len() as u64).saturating_add(plain.len() as u64) > manifest.size() {
                        return Err(NodeError::File("file size mismatch".into()));
                    }
                    out.extend_from_slice(&plain);
                }
                if out.len() as u64 != manifest.size() {
                    return Err(NodeError::File("file size mismatch".into()));
                }
                if file_checksum(&out) != manifest.checksum() {
                    return Err(NodeError::File("checksum mismatch".into()));
                }
                Ok(out)
            }
        }
    }

    /// Save a received file into `dir`, deriving the filename from the (remote-supplied)
    /// manifest name. The name is sanitized — directory components stripped, traversal /
    /// absolute / drive prefixes rejected, OS-illegal chars legalized — and the final
    /// path is confirmed to stay within `dir`, de-duplicating with a `name (N).ext`
    /// counter so two saves never clobber. Returns the actual path written.
    ///
    /// This is the safe entry point for a directory-based "save to Downloads" flow:
    /// the caller supplies a TRUSTED directory and the file's own name is never trusted
    /// to escape it.
    pub fn save_file_into_dir(
        &self,
        file_conv: ConversationId,
        dir: &Path,
    ) -> Result<std::path::PathBuf, NodeError> {
        let name = self
            .files
            .lock()
            .expect("files mutex not poisoned")
            .manifest(&file_conv)
            .map(|m| m.name().to_string())
            .ok_or_else(|| NodeError::File("unknown file".into()))?;
        let dest = crate::util::savename::safe_save_path(dir, &name).ok_or_else(|| {
            NodeError::File("could not place file safely within directory".into())
        })?;
        self.save_file_progress(file_conv, &dest, |_| {})?;
        Ok(dest)
    }

    /// Save a received file to `dest`, streaming chunk-by-chunk so the whole file is
    /// never buffered. Writes to `dest.part`, verifying each chunk's hash + AEAD on the
    /// way and the whole-file checksum at the end, then atomically renames into place.
    /// A v1 (legacy) file falls back to the in-memory reassemble path.
    pub fn save_file(&self, file_conv: ConversationId, dest: &Path) -> Result<(), NodeError> {
        self.save_file_progress(file_conv, dest, |_| {})
    }

    /// [`Node::save_file`] with a progress callback (one call per written chunk, plus a
    /// terminal `done == total`).
    pub fn save_file_progress(
        &self,
        file_conv: ConversationId,
        dest: &Path,
        mut on_progress: impl FnMut(FileProgress),
    ) -> Result<(), NodeError> {
        let manifest = self
            .files
            .lock()
            .expect("files mutex not poisoned")
            .manifest(&file_conv)
            .cloned()
            .ok_or_else(|| NodeError::File("unknown file".into()))?;
        let total = manifest.chunk_count();
        let candidates = self.completion_candidates(file_conv);
        let chunks = self.collect_chunks(file_conv);
        if chunks.len() as u32 != total {
            if let Some(source) = self.managed_media_for_export(file_conv, &manifest) {
                let part = part_path(dest);
                let mut guard = PartFileGuard::new(part.clone());
                let mut reader = std::io::BufReader::new(
                    std::fs::File::open(source)
                        .map_err(|e| NodeError::File(format!("open media: {e}")))?,
                );
                let mut output = std::fs::File::create(&part)
                    .map_err(|e| NodeError::File(format!("create part: {e}")))?;
                let mut checksum = Sha256::new();
                let mut size = 0u64;
                let mut buffer = vec![0; 64 * 1024];
                loop {
                    let count = reader
                        .read(&mut buffer)
                        .map_err(|e| NodeError::File(format!("read media: {e}")))?;
                    if count == 0 {
                        break;
                    }
                    size += count as u64;
                    if size > manifest.size() {
                        return Err(NodeError::File("media size mismatch".into()));
                    }
                    checksum.update(&buffer[..count]);
                    output
                        .write_all(&buffer[..count])
                        .map_err(|e| NodeError::File(format!("write media: {e}")))?;
                }
                let actual: [u8; 32] = checksum.finalize().into();
                if actual != manifest.checksum() || size != manifest.size() {
                    return Err(NodeError::File("media checksum mismatch".into()));
                }
                output
                    .sync_all()
                    .map_err(|e| NodeError::File(format!("sync media: {e}")))?;
                std::fs::rename(&part, dest)
                    .map_err(|e| NodeError::File(format!("finalize rename: {e}")))?;
                guard.disarm();
                super::media_store::sync_parent(dest)
                    .map_err(|e| NodeError::File(format!("sync directory: {e}")))?;
                on_progress(FileProgress { done: total, total });
                return Ok(());
            }
            return Err(NodeError::File(format!(
                "file incomplete: {}/{} chunks",
                chunks.len(),
                total
            )));
        }

        // v1 has no per-chunk hash / deterministic nonce: reassemble in memory (these
        // are the old <=8 MB single-blob-era files) and write.
        if let AnyManifest::V1(m) = &manifest {
            let data = reassemble_and_verify(m, &chunks)
                .map_err(|e| NodeError::File(format!("reassemble: {e}")))?;
            std::fs::write(dest, data).map_err(|e| NodeError::File(format!("write: {e}")))?;
            on_progress(FileProgress { done: total, total });
            return Ok(());
        }

        // v2: stream each verified chunk straight to a temp .part file. Guard the temp so
        // ANY early-return error path (chunk open, write/flush IO error, checksum mismatch,
        // even the final rename) removes it instead of orphaning a `.part` on disk; the
        // guard is disarmed only once the file is committed into place.
        let part = part_path(dest);
        let mut part_guard = PartFileGuard::new(part.clone());
        let mut whole = Sha256::new();
        let mut size = 0_u64;
        {
            let f = std::fs::File::create(&part)
                .map_err(|e| NodeError::File(format!("create part: {e}")))?;
            let mut writer = std::io::BufWriter::new(f);
            for (i, ct) in chunks.iter().enumerate() {
                let plain = open_chunk_for(&manifest, i as u32, ct)
                    .map_err(|e| NodeError::File(format!("open chunk {i}: {e}")))?;
                size = size.saturating_add(plain.len() as u64);
                if size > manifest.size() {
                    return Err(NodeError::File("file size mismatch".into()));
                }
                whole.update(&plain);
                writer
                    .write_all(&plain)
                    .map_err(|e| NodeError::File(format!("write chunk {i}: {e}")))?;
                on_progress(FileProgress {
                    done: i as u32 + 1,
                    total,
                });
            }
            if size != manifest.size() {
                return Err(NodeError::File("file size mismatch".into()));
            }
            writer
                .flush()
                .map_err(|e| NodeError::File(format!("flush: {e}")))?;
            writer
                .get_ref()
                .sync_all()
                .map_err(|e| NodeError::File(format!("sync file: {e}")))?;
        }
        let expected = manifest.checksum();
        let actual: [u8; 32] = whole.finalize().into();
        if actual != expected {
            return Err(NodeError::File("whole-file checksum mismatch".into()));
        }
        std::fs::rename(&part, dest)
            .map_err(|e| NodeError::File(format!("finalize rename: {e}")))?;
        // Committed into place: don't let the guard delete the now-renamed file.
        part_guard.disarm();
        super::media_store::sync_parent(dest)
            .map_err(|e| NodeError::File(format!("sync directory: {e}")))?;
        // The file is fully reassembled + verified on disk: reclaim its chunk events.
        self.commit_file_completion(file_conv, &candidates);
        Ok(())
    }

    /// The chunk-event ciphertexts of a per-file conversation, in log order.
    fn collect_chunks(&self, file_conv: ConversationId) -> Vec<Vec<u8>> {
        let log = self.log.lock().expect("log mutex not poisoned");
        log.events(&file_conv)
            .into_iter()
            .filter(|e| e.kind == EventKind::Message)
            .map(|e| e.ciphertext.clone())
            .collect()
    }
}

/// Read exactly `buf.len()` bytes, or fewer only at EOF (the final chunk). `BufReader`
/// can return short reads mid-stream, so loop until full or EOF.
fn read_full<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// The temp path a save streams into before its atomic rename: `dest` + `.part`.
fn part_path(dest: &Path) -> std::path::PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".part");
    std::path::PathBuf::from(s)
}

/// Removes a half-written `.part` temp on drop unless [`Self::disarm`]ed. Ensures every
/// error return from a streaming save (chunk open, write/flush failure, checksum mismatch,
/// rename failure) cleans up its temp instead of orphaning it.
struct PartFileGuard {
    path: std::path::PathBuf,
    armed: bool,
}

impl PartFileGuard {
    fn new(path: std::path::PathBuf) -> Self {
        Self { path, armed: true }
    }

    /// Stop the guard from deleting the temp (call once it's been committed into place).
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PartFileGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Best-effort MIME from a file name's extension, for the manifest. The UI also has its own
/// fallback, but a correct manifest MIME lets a typed blob (and any future consumer) pick a
/// decoder. Unknown extensions stay `application/octet-stream`.
pub(in crate::node) fn mime_from_name(name: &str) -> String {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "m4v" => "video/x-m4v",
        "ogv" => "video/ogg",
        _ => "application/octet-stream",
    };
    mime.to_string()
}

#[cfg(test)]
mod mime_tests {
    use super::mime_from_name;

    #[test]
    fn maps_known_extensions_and_falls_back() {
        assert_eq!(mime_from_name("clip.mp4"), "video/mp4");
        assert_eq!(mime_from_name("IMG_0001.MOV"), "video/quicktime"); // case-insensitive
        assert_eq!(mime_from_name("a.b.webm"), "video/webm");
        assert_eq!(mime_from_name("photo.png"), "image/png");
        assert_eq!(mime_from_name("IMG_0001.HEIC"), "image/heic");
        assert_eq!(mime_from_name("report.pdf"), "application/octet-stream");
        assert_eq!(mime_from_name("noext"), "application/octet-stream");
    }
}

#[cfg(test)]
mod part_guard_tests {
    use super::PartFileGuard;

    #[test]
    fn armed_guard_removes_the_part_on_drop() {
        // An error path drops the guard while still armed → the temp must be gone (no
        // orphaned `.part` left behind on any failed save).
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("file.bin.part");
        std::fs::write(&part, b"half-written").unwrap();
        {
            let _guard = PartFileGuard::new(part.clone());
            assert!(part.exists());
        }
        assert!(!part.exists(), "armed guard must remove the .part on drop");
    }

    #[test]
    fn disarmed_guard_keeps_the_file() {
        // The success path disarms after the atomic rename → the committed file survives.
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("file.bin.part");
        std::fs::write(&part, b"committed").unwrap();
        {
            let mut guard = PartFileGuard::new(part.clone());
            guard.disarm();
        }
        assert!(part.exists(), "disarmed guard must NOT remove the file");
    }
}
