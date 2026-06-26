//! File transfer: chunked send (DM/account/channel) and save, streamed to/from disk
//! so neither side ever buffers the whole file. Files become a v2 (`MFM2`) manifest
//! plus one chunk event per `CHUNK_SIZE` piece in a dedicated per-file conversation;
//! the chunk events ride the normal bounded sync, which already re-requests only the
//! events a peer is missing — so transfer RESUMES across reconnects for free.

use super::node::MAX_FILE_SIZE;
use super::*;
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

/// Progress of a file transfer: `done`/`total` chunks. The terminal callback always
/// has `done == total`.
#[derive(Debug, Clone, Copy)]
pub struct FileProgress {
    pub done: u32,
    pub total: u32,
}

impl Node {
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
        let peer = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .get(recipient)
            .cloned()
            .ok_or_else(|| NodeError::UnknownPeer(recipient.to_string()))?;

        let (manifest, file_conv) = self.stage_file_blocking(path, kind, on_progress).await?;
        let sealed = crate::dm::seal(&self.identity, &peer.public.x25519_pub, &manifest.encode())
            .map_err(NodeError::Seal)?;
        let dm_conv = dm_conversation_id(&self.identity.public(), &peer.public);
        let seq = self.append_event(dm_conv, EventKind::FileManifest, sealed)?;
        self.record_sent_manifest(dm_conv, dm_conv, seq, &manifest);

        self.deliver_direct(&peer, file_conv).await.ok();
        self.replicate_to_post_office(file_conv).await.ok();
        self.deliver_direct(&peer, dm_conv).await.ok();
        self.replicate_to_post_office(dm_conv).await.ok();
        Ok(file_conv)
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
        let dests = self.account_fanout_targets(target_account_id);
        if !dests
            .iter()
            .any(|p| p.account_id.as_deref() == Some(target_account_id))
        {
            return Err(NodeError::UnknownPeer(target_account_id.to_string()));
        }

        let (manifest, file_conv) = self.stage_file_blocking(path, kind, on_progress).await?;
        let manifest_bytes = manifest.encode();
        // Record the outgoing file ONCE under the account conversation (the UI's host
        // conversation for this contact), so it shows as our own message in account
        // history. The per-device manifest events below each get a distinct event id; we
        // record the first that lands (its id keys the durable store) rather than once per
        // device — which would duplicate the bubble.
        let account_conv = super::conversation::account_conversation_id(
            &self.account.account_id(),
            target_account_id,
        );
        let mut recorded = false;
        for peer in &dests {
            let sealed =
                match crate::dm::seal(&self.identity, &peer.public.x25519_pub, &manifest_bytes) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
            let dm_conv = dm_conversation_id(&self.identity.public(), &peer.public);
            let seq = match self.append_event(dm_conv, EventKind::FileManifest, sealed) {
                Ok(seq) => seq,
                Err(_) => continue,
            };
            if !recorded {
                self.record_sent_manifest(account_conv, dm_conv, seq, &manifest);
                recorded = true;
            }
            self.deliver_direct(peer, file_conv).await.ok();
            self.replicate_to_post_office(file_conv).await.ok();
            self.deliver_direct(peer, dm_conv).await.ok();
            self.replicate_to_post_office(dm_conv).await.ok();
        }
        Ok(file_conv)
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
        self.record_sent_manifest(channel, channel, seq, &manifest);
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
            return Err(NodeError::File(format!(
                "file too large: {size} bytes (max {MAX_FILE_SIZE})"
            )));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());

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
    ) {
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
            return;
        };
        let plaintext = manifest.encode();
        let _ = self
            .received_files
            .lock()
            .expect("received_files mutex not poisoned")
            .record(
                record_conv,
                self.identity.public().user_id(),
                wall_clock,
                &plaintext,
                event_id,
            );
        let mut files = self.files.lock().expect("files mutex not poisoned");
        files.mark_emitted(event_id);
        files.record(AnyManifest::V3(manifest.clone()));
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
        if self.media.contains(file_conv) {
            return false; // already durable
        }
        // Need all chunks present; `read_file` enforces completeness + verifies integrity.
        match self.read_file(file_conv) {
            Ok(bytes) => {
                if self.media.store_bytes(file_conv, &name, &bytes).is_ok() {
                    // The durable media copy now exists, so the transient chunks are
                    // reclaimable — mirror save_file's prune (history still shows the bubble
                    // from the FileBook + received_files; the preview loads from the store).
                    self.prune_file_chunks(file_conv);
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
                    out.extend_from_slice(
                        &open_chunk_for(&manifest, i as u32, ct)
                            .map_err(|e| NodeError::File(format!("open chunk {i}: {e}")))?,
                    );
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
        let chunks = self.collect_chunks(file_conv);
        if chunks.len() as u32 != total {
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
        {
            let f = std::fs::File::create(&part)
                .map_err(|e| NodeError::File(format!("create part: {e}")))?;
            let mut writer = std::io::BufWriter::new(f);
            for (i, ct) in chunks.iter().enumerate() {
                let plain = open_chunk_for(&manifest, i as u32, ct)
                    .map_err(|e| NodeError::File(format!("open chunk {i}: {e}")))?;
                whole.update(&plain);
                writer
                    .write_all(&plain)
                    .map_err(|e| NodeError::File(format!("write chunk {i}: {e}")))?;
                on_progress(FileProgress {
                    done: i as u32 + 1,
                    total,
                });
            }
            writer
                .flush()
                .map_err(|e| NodeError::File(format!("flush: {e}")))?;
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
        // The file is fully reassembled + verified on disk: reclaim its chunk events.
        self.prune_file_chunks(file_conv);
        Ok(())
    }

    /// Drop a completed file's CHUNK events from the durable event log (one event per
    /// CHUNK_SIZE piece, otherwise kept append-only forever). Best-effort: a failure to
    /// compact is non-fatal (the file was already saved). Only the per-file chunk
    /// conversation is dropped — the manifest stays in the DM/channel conversation, and
    /// the manifest entry stays in the FileBook, so history still shows the attachment;
    /// progress simply reports complete from the manifest's chunk_count vs. zero held
    /// (a re-save would re-sync the chunks if a peer still has them).
    fn prune_file_chunks(&self, file_conv: ConversationId) {
        let mut log = self.log.lock().expect("log mutex not poisoned");
        let _ = log.drop_conversation(&file_conv);
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
