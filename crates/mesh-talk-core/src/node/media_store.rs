//! The chat-media store: a DURABLE, encrypted copy of image/screenshot/video bytes that
//! belong to a chat message, kept SEPARATE from the transient chunked file-transfer path.
//!
//! Generic file attachments ride the chunked event-log transfer and are saved to a
//! user-chosen location on demand (their chunks are pruned afterwards). Media messages
//! (images/screenshots/videos) are instead copied here on send / receive-complete so the
//! inline preview always loads from this store — surviving chunk prune AND restart, where
//! the old preview read the soon-to-be-pruned transient chunks and broke.
//!
//! Layout: `<account_dir>/media/<file_conv_hex>.<ext>`. Keying by the per-file
//! conversation id (globally unique per file) means a [`crate::node::HistoryEntry`]'s
//! `file_conv` maps DIRECTLY to its stored file with no conversation lookup, and the
//! per-account dir keeps multi-account installs from colliding. The extension is derived
//! from the original filename so the asset/`<img>` layer can sniff the type; the lookup
//! globs on the stem so the accessor needs only the `file_conv`.

use crate::eventlog::event::ConversationId;
use crate::storage::encryption::{EncryptionKey, generate_salt};
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use rand_core::RngCore;
use std::collections::HashSet;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

mod indexed;

const MAGIC: &[u8; 8] = b"MTMEDIA1";
const CHUNK: usize = 64 * 1024;

/// Image/screenshot + video extensions treated as inline chat media. Mirrors the
/// frontend's `isImage`/`isVideo` (`mediaFile.tsx`) so the send-side media decision and the
/// display-side render decision agree. Manifest `mime` is not populated end-to-end today
/// (it stays `application/octet-stream`), so the filename extension is the reliable signal.
const MEDIA_EXT: &[&str] = &[
    // images
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "avif", "svg", // videos
    "mp4", "mov", "webm", "m4v", "ogv",
];

/// Whether a file `name` is chat media (image/screenshot/video) that should be persisted to
/// the media store and rendered inline — as opposed to a generic attachment (download card).
/// Extension-based to match the frontend's render decision exactly.
pub fn is_media_name(name: &str) -> bool {
    ext_of(name).is_some_and(|e| MEDIA_EXT.contains(&e.as_str()))
}

/// Whether a file is inline MEDIA. Authoritative source is the sender's intent (the
/// manifest `kind`, set by which button sent it); only when that's absent — legacy v1/v2
/// manifests from older peers — do we fall back to the filename heuristic. This is what
/// makes a `.mov` sent via the ATTACH button a downloadable attachment, not inline media.
pub fn manifest_is_media(m: &crate::file::AnyManifest) -> bool {
    match m.kind() {
        Some(crate::file::FileKind::Media) => true,
        Some(crate::file::FileKind::File) => false,
        None => is_media_name(m.name()),
    }
}

/// The no-prompt lane is bounded and limited to raster image formats. A sender's
/// filename/MIME is still metadata, so this does not assert that bytes decode as
/// an image; completion verifies the signed hashes before display or export.
pub fn automatic_inline_image(m: &crate::file::AnyManifest) -> bool {
    manifest_is_media(m)
        && m.size() <= 16 * 1024 * 1024
        && matches!(
            m.mime(),
            "image/png"
                | "image/jpeg"
                | "image/gif"
                | "image/webp"
                | "image/bmp"
                | "image/avif"
                | "image/heic"
                | "image/heif"
        )
}

/// Reject files renamed to a raster extension before they enter the automatic
/// receive lane. This is a format signature check, not a full image decoder.
pub fn raster_signature_matches(mime: &str, bytes: &[u8]) -> bool {
    match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        "image/bmp" => bytes.starts_with(b"BM"),
        "image/avif" => {
            bytes.get(4..12) == Some(b"ftypavif") || bytes.get(4..12) == Some(b"ftypavis")
        }
        "image/heic" | "image/heif" => {
            bytes.get(4..8) == Some(b"ftyp")
                && bytes.get(8..12).is_some_and(|brand| {
                    [b"heic", b"heix", b"hevc", b"mif1"]
                        .iter()
                        .any(|known| brand == *known)
                })
        }
        _ => false,
    }
}

/// The lowercased extension of `name`, if any (no leading dot).
fn ext_of(name: &str) -> Option<String> {
    Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
}

#[cfg(test)]
type PrepareHook = Arc<Mutex<Option<Box<dyn FnOnce() + Send + Sync>>>>;

/// The durable chat-media store rooted at `<account_dir>/media`. Cheap to clone (just the
/// dir); all state is on disk.
#[derive(Clone)]
pub struct MediaStore {
    dir: PathBuf,
    key: Arc<EncryptionKey>,
    owner_id: String,
    legacy_unmigrated: Arc<Mutex<HashSet<ConversationId>>>,
    index: Option<Arc<indexed::MediaIndex>>,
    mutation: Arc<Mutex<()>>,
    entry_limit: Option<usize>,
    #[cfg(test)]
    prepare_hook: PrepareHook,
}

/// Encrypted and verified bytes that remain invisible until the owner commits.
pub(in crate::node) struct PreparedMediaWrite {
    directory: tempfile::TempDir,
    destination: PathBuf,
    conversation: ConversationId,
    key: Arc<EncryptionKey>,
    owner_id: String,
}

impl MediaStore {
    #[cfg(test)]
    pub(in crate::node) fn before_prepare_for_test(&self, hook: Box<dyn FnOnce() + Send + Sync>) {
        *self.prepare_hook.lock().unwrap() = Some(hook);
    }

    /// Open (creating if needed) the media store under `account_dir`.
    pub fn open(account_dir: &Path, password: &str, owner_id: &str) -> std::io::Result<Self> {
        Self::open_inner(account_dir, password, owner_id, None, None)
    }
    fn open_inner(
        account_dir: &Path,
        password: &str,
        owner_id: &str,
        profile: Option<Arc<crate::storage::profile::context::ProfileStorage>>,
        entry_limit: Option<usize>,
    ) -> std::io::Result<Self> {
        let dir = account_dir.join("media");
        std::fs::create_dir_all(&dir)?;
        set_private_dir(&dir)?;
        let salt_path = dir.join("key.salt");
        let salt = if salt_path.exists() {
            if !std::fs::symlink_metadata(&salt_path)?.file_type().is_file() {
                return Err(invalid("media salt is not a regular file"));
            }
            let bytes = std::fs::read(&salt_path)?;
            bytes
                .try_into()
                .map_err(|_| invalid("invalid media key salt"))?
        } else {
            let salt = generate_salt();
            let mut file = private_create_new(&salt_path)?;
            file.write_all(&salt)?;
            file.sync_all()?;
            sync_parent(&salt_path)?;
            salt
        };
        let key = EncryptionKey::from_password(password, &salt)
            .map_err(|e| invalid(&format!("media key derivation: {e}")))?;
        let index = profile
            .map(|profile| indexed::MediaIndex::open(profile, &dir, owner_id, entry_limit))
            .transpose()?
            .map(Arc::new);
        let mut store = Self {
            dir,
            index,
            mutation: Arc::new(Mutex::new(())),
            entry_limit,
            key: Arc::new(key),
            owner_id: owner_id.into(),
            legacy_unmigrated: Arc::new(Mutex::new(HashSet::new())),
            #[cfg(test)]
            prepare_hook: Arc::new(Mutex::new(None)),
        };
        if store.index.is_some() {
            store.indexed_inventory()?;
        } else {
            store.migrate_plaintext()?;
        }
        Ok(store)
    }

    /// Enumerate only regular files named for a managed file conversation.
    /// Symlinks and incomplete `.part` writes are never backup sources.
    fn legacy_managed_files(&self) -> std::io::Result<Vec<(ConversationId, PathBuf)>> {
        let mut files = std::collections::BTreeMap::new();
        for entry in std::fs::read_dir(&self.dir)? {
            #[cfg(test)]
            directory_observations::visit();
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if stem.len() != 64 {
                continue;
            }
            let mut id = [0u8; 32];
            if hex::decode_to_slice(stem, &mut id).is_err() {
                continue;
            }
            if files.insert(id, path).is_some() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "duplicate managed media ID",
                ));
            }
        }
        Ok(files
            .into_iter()
            .map(|(id, path)| (ConversationId::new(id), path))
            .collect())
    }

    /// The on-disk path a `file_conv` of media `name` is stored at: `<dir>/<hex>.<ext>`.
    pub(in crate::node) fn path_for(&self, file_conv: ConversationId, name: &str) -> PathBuf {
        let hex = hex::encode(file_conv.as_bytes());
        match ext_of(name) {
            Some(ext) => self.dir.join(format!("{hex}.{ext}")),
            None => self.dir.join(hex),
        }
    }

    /// Resolve the stored file for `file_conv` regardless of extension (the accessor knows
    /// only the conv id). Returns the first match whose stem is the conv hex.
    pub fn path(&self, file_conv: ConversationId) -> Option<PathBuf> {
        match self.selected_file(file_conv) {
            Ok(file) => file.map(|(path, _)| path),
            Err(error) => {
                log::warn!("media lookup failed: {error}");
                None
            }
        }
    }

    /// Whether media bytes for `file_conv` are durably stored.
    pub fn contains(&self, file_conv: ConversationId) -> bool {
        self.path(file_conv).is_some()
    }

    pub fn migration_warning(&self) -> bool {
        if let Some(index) = &self.index {
            return index.has_legacy().unwrap_or_else(|error| {
                log::warn!("media migration observation failed: {error}");
                true
            });
        }
        self.legacy_unmigrated
            .lock()
            .is_ok_and(|items| !items.is_empty())
    }

    pub fn remove(&self, file_conv: ConversationId) -> std::io::Result<()> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| invalid("media mutation lock poisoned"))?;
        if let Some(index) = &self.index {
            loop {
                let page = index.exact(file_conv, None)?;
                if page.is_empty() {
                    return Ok(());
                }
                for entry in page {
                    let path = self.dir.join(&entry.name);
                    match std::fs::symlink_metadata(&path) {
                        Ok(meta) if meta.is_file() => {
                            std::fs::remove_file(&path)?;
                            sync_parent(&path)?;
                        }
                        Ok(_) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
                    }
                    index.remove(&entry.name, Some(entry.revision))?;
                }
            }
        }
        let hex = hex::encode(file_conv.as_bytes());
        let mut removed_path = None;
        for entry in std::fs::read_dir(&self.dir)? {
            #[cfg(test)]
            directory_observations::visit();
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            if path.file_stem().and_then(|stem| stem.to_str()) == Some(hex.as_str()) {
                std::fs::remove_file(&path)?;
                removed_path = Some(path);
            }
        }
        if let Some(path) = removed_path {
            sync_parent(&path)?;
        }
        if let Ok(mut legacy) = self.legacy_unmigrated.lock() {
            legacy.remove(&file_conv);
        }
        Ok(())
    }

    /// Recover from a crash between local card erasure and managed-file removal.
    pub fn prune_unreferenced(&self, live: &HashSet<ConversationId>) -> std::io::Result<()> {
        self.prune_unreferenced_where(|file| Ok(live.contains(&file)))
    }

    pub(in crate::node) fn prune_unreferenced_where(
        &self,
        mut live: impl FnMut(ConversationId) -> std::io::Result<bool>,
    ) -> std::io::Result<()> {
        if let Some(index) = &self.index {
            let mut after = None;
            loop {
                let page = index.page(after.as_deref(), None)?;
                if page.is_empty() {
                    return Ok(());
                }
                for entry in page {
                    after = Some(entry.name.clone());
                    let _guard = self
                        .mutation
                        .lock()
                        .map_err(|_| invalid("media mutation lock poisoned"))?;
                    if index.unchanged(&entry)? && (entry.partial || !live(entry.file)?) {
                        let path = self.dir.join(&entry.name);
                        match std::fs::symlink_metadata(&path) {
                            Ok(meta) if meta.is_file() => {
                                std::fs::remove_file(&path)?;
                                sync_parent(&path)?;
                            }
                            Ok(_) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                            Err(error) => return Err(error),
                        }
                        index.remove(&entry.name, Some(entry.revision))?;
                    }
                }
            }
        }
        let mut removed_path = None;
        for entry in std::fs::read_dir(&self.dir)? {
            #[cfg(test)]
            directory_observations::visit();
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            let Ok(bytes) = hex::decode(stem) else {
                continue;
            };
            let Ok(bytes): Result<[u8; 32], _> = bytes.try_into() else {
                continue;
            };
            let conv = ConversationId::new(bytes);
            if !live(conv)? {
                // The directory entry already identifies the exact managed file.
                // Rescanning the whole directory for each orphan is quadratic.
                std::fs::remove_file(&path)?;
                removed_path = Some(path);
                if let Ok(mut legacy) = self.legacy_unmigrated.lock() {
                    legacy.remove(&conv);
                }
            }
        }
        if let Some(path) = removed_path {
            sync_parent(&path)?;
        }
        Ok(())
    }

    /// Copy the file at `src` into the store under `file_conv` (sender side: the source
    /// path is on disk). Streamed so a large media file is never fully buffered. Idempotent:
    /// re-storing the same conv overwrites. A no-op-safe error is returned for IO failures.
    pub fn store_from_path(
        &self,
        file_conv: ConversationId,
        name: &str,
        src: &Path,
    ) -> std::io::Result<PathBuf> {
        let dest = self.path_for(file_conv, name);
        // Stream src -> dest.part -> rename, so a crash mid-copy never leaves a truncated
        // file masquerading as complete.
        let mut reader = std::io::BufReader::new(std::fs::File::open(src)?);
        let size = std::fs::metadata(src)?.len();
        self.store_stream(file_conv, &dest, &mut reader, size)?;
        Ok(dest)
    }

    /// Perform bulk encryption and verification before acquiring a caller's
    /// commit guard. The token cannot expose a managed path before publication.
    pub(in crate::node) fn prepare_from_path_if(
        &self,
        conversation: ConversationId,
        name: &str,
        source: &Path,
        mut check: impl FnMut() -> std::io::Result<()>,
    ) -> std::io::Result<PreparedMediaWrite> {
        let directory = tempfile::Builder::new()
            .prefix(".pending-media-")
            .tempdir_in(&self.dir)?;
        let mut reader = std::io::BufReader::new(std::fs::File::open(source)?);
        let total = std::fs::metadata(source)?.len();
        self.encrypt_stream(
            conversation,
            &directory.path().join("ciphertext.part"),
            &mut reader,
            total,
            &mut check,
        )?;
        Ok(PreparedMediaWrite {
            directory,
            destination: self.path_for(conversation, name),
            conversation,
            key: Arc::clone(&self.key),
            owner_id: self.owner_id.clone(),
        })
    }

    /// Caller has rechecked authorization under its existing commit guard.
    pub(in crate::node) fn publish_prepared(
        &self,
        prepared: PreparedMediaWrite,
    ) -> std::io::Result<PathBuf> {
        if prepared.owner_id != self.owner_id
            || !Arc::ptr_eq(&prepared.key, &self.key)
            || prepared.destination.parent() != Some(self.dir.as_path())
        {
            return Err(invalid("prepared media belongs to another storage owner"));
        }
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| invalid("media mutation lock poisoned"))?;
        if let Some(index) = &self.index {
            index.register(
                self.indexed_name(&prepared.destination)?,
                prepared.conversation,
                false,
                None,
            )?;
        }
        std::fs::rename(
            prepared.directory.path().join("ciphertext.part"),
            &prepared.destination,
        )?;
        sync_parent(&prepared.destination)?;
        if let Some(index) = &self.index {
            index.complete(self.indexed_name(&prepared.destination)?)?;
        }
        if let Ok(mut legacy) = self.legacy_unmigrated.lock() {
            legacy.remove(&prepared.conversation);
        }
        Ok(prepared.destination.clone())
    }

    /// Write already-decrypted, already-verified `bytes` into the store under `file_conv`
    /// (receive-complete side: the reassembled bytes are in hand). Atomic via a `.part`
    /// rename. Idempotent.
    pub fn store_bytes(
        &self,
        file_conv: ConversationId,
        name: &str,
        bytes: &[u8],
    ) -> std::io::Result<PathBuf> {
        let dest = self.path_for(file_conv, name);
        let mut reader = bytes;
        self.store_stream(file_conv, &dest, &mut reader, bytes.len() as u64)?;
        Ok(dest)
    }

    /// Read the stored media bytes for `file_conv` (used by the `read_media` IPC accessor).
    /// Refuses to load a file larger than [`MAX_INLINE_MEDIA_BYTES`] fully into memory — an
    /// oversized media file (up to the 4 GiB file cap) would OOM the process; the UI gates
    /// previews well under this, so a `None` here just means "no inline preview".
    pub fn read(&self, file_conv: ConversationId) -> Option<Vec<u8>> {
        let (path, eligible) = self.selected_file(file_conv).ok()??;
        let legacy = Self::plaintext_source(&path, eligible).ok()?;
        let size = if legacy {
            std::fs::metadata(&path).ok()?.len()
        } else {
            self.declared_size(&path).ok()?
        };
        if size > MAX_INLINE_MEDIA_BYTES {
            return None;
        }
        let mut bytes = Vec::with_capacity(size as usize);
        self.copy_decrypted(file_conv, &mut bytes).ok()?;
        Some(bytes)
    }

    pub fn copy_decrypted(
        &self,
        file_conv: ConversationId,
        writer: &mut impl Write,
    ) -> std::io::Result<u64> {
        let (path, eligible) = self
            .selected_file(file_conv)?
            .ok_or_else(|| invalid("media missing"))?;
        if Self::plaintext_source(&path, eligible)? {
            return std::io::copy(&mut std::fs::File::open(path)?, writer);
        }
        let mut file = std::io::BufReader::new(std::fs::File::open(&path)?);
        let mut magic = [0; 8];
        file.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(invalid("unencrypted media"));
        }
        let mut prefix = [0; 8];
        file.read_exact(&mut prefix)?;
        let mut size_bytes = [0; 8];
        file.read_exact(&mut size_bytes)?;
        let total = u64::from_le_bytes(size_bytes);
        let cipher = Aes256Gcm::new(self.key.as_bytes().into());
        let mut done = 0u64;
        let mut index = 0u32;
        while done < total || index == 0 {
            let len = ((total - done).min(CHUNK as u64)) as usize;
            let mut ciphertext = vec![0u8; len + 16];
            file.read_exact(&mut ciphertext)?;
            let nonce = nonce(prefix, index);
            let nonce =
                Nonce::try_from(nonce.as_slice()).map_err(|_| invalid("invalid media nonce"))?;
            let aad = self.aad(file_conv, total, index, len as u32);
            let plain = cipher
                .decrypt(
                    &nonce,
                    Payload {
                        msg: &ciphertext,
                        aad: &aad,
                    },
                )
                .map_err(|_| invalid("media authentication failed"))?;
            writer.write_all(&plain)?;
            done += plain.len() as u64;
            index = index
                .checked_add(1)
                .ok_or_else(|| invalid("media too large"))?;
        }
        if file.read(&mut [0u8; 1])? != 0 {
            return Err(invalid("trailing media bytes"));
        }
        Ok(done)
    }

    fn declared_size(&self, path: &Path) -> std::io::Result<u64> {
        let mut file = std::fs::File::open(path)?;
        let mut header = [0u8; 24];
        file.read_exact(&mut header)?;
        if &header[..8] != MAGIC {
            return Err(invalid("unencrypted media"));
        }
        Ok(u64::from_le_bytes(header[16..24].try_into().unwrap()))
    }

    fn aad(&self, conv: ConversationId, total: u64, index: u32, len: u32) -> Vec<u8> {
        let mut aad = Vec::with_capacity(self.owner_id.len() + 52);
        aad.extend_from_slice(self.owner_id.as_bytes());
        aad.extend_from_slice(conv.as_bytes());
        aad.extend_from_slice(&total.to_le_bytes());
        aad.extend_from_slice(&index.to_le_bytes());
        aad.extend_from_slice(&len.to_le_bytes());
        aad
    }

    fn store_stream(
        &self,
        conv: ConversationId,
        dest: &Path,
        reader: &mut impl Read,
        total: u64,
    ) -> std::io::Result<()> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| invalid("media mutation lock poisoned"))?;
        self.store_stream_locked(conv, dest, reader, total)
    }

    // The caller already holds the mutation lock, including when maintenance
    // retries a plaintext row while its reference owner remains locked.
    fn store_stream_locked(
        &self,
        conv: ConversationId,
        dest: &Path,
        reader: &mut impl Read,
        total: u64,
    ) -> std::io::Result<()> {
        let part = part_path(dest);
        if let Some(index) = &self.index {
            index.register(self.indexed_name(dest)?, conv, false, None)?;
            index.register(self.indexed_name(&part)?, conv, true, Some(false))?;
        }
        self.encrypt_stream(conv, &part, reader, total, &mut || Ok(()))?;
        std::fs::rename(&part, dest)?;
        sync_parent(dest)?;
        if let Some(index) = &self.index {
            index.complete(self.indexed_name(dest)?)?;
            index.remove(self.indexed_name(&part)?, None)?;
        }
        if let Ok(mut legacy) = self.legacy_unmigrated.lock() {
            legacy.remove(&conv);
        }
        Ok(())
    }

    fn encrypt_stream(
        &self,
        conv: ConversationId,
        part: &Path,
        reader: &mut impl Read,
        total: u64,
        check: &mut impl FnMut() -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        #[cfg(test)]
        {
            let hook = self.prepare_hook.lock().unwrap().take();
            if let Some(hook) = hook {
                hook();
            }
        }
        let mut file = private_create(part)?;
        let mut prefix = [0u8; 8];
        rand_core::OsRng.fill_bytes(&mut prefix);
        file.write_all(MAGIC)?;
        file.write_all(&prefix)?;
        file.write_all(&total.to_le_bytes())?;
        let cipher = Aes256Gcm::new(self.key.as_bytes().into());
        let mut done = 0u64;
        let mut index = 0u32;
        let mut buffer = vec![0u8; CHUNK];
        while done < total || index == 0 {
            check()?;
            // An empty payload gets one authenticated chunk. Every later pass must
            // still have plaintext to consume; otherwise this loop can write tags
            // indefinitely without making progress.
            if index != 0 && done == total {
                return Err(invalid("media encryption made no progress"));
            }
            let len = ((total - done).min(CHUNK as u64)) as usize;
            reader.read_exact(&mut buffer[..len])?;
            let aad = self.aad(conv, total, index, len as u32);
            let nonce_bytes = nonce(prefix, index);
            let nonce = Nonce::try_from(nonce_bytes.as_slice())
                .map_err(|_| invalid("invalid media nonce"))?;
            let encrypted = cipher
                .encrypt(
                    &nonce,
                    Payload {
                        msg: &buffer[..len],
                        aad: &aad,
                    },
                )
                .map_err(|_| invalid("media encryption failed"))?;
            file.write_all(&encrypted)?;
            done += len as u64;
            index = index
                .checked_add(1)
                .ok_or_else(|| invalid("media too large"))?;
        }
        file.sync_all()?;
        self.verify_file(part, conv, total, check)
    }

    fn verify_file(
        &self,
        path: &Path,
        conv: ConversationId,
        total: u64,
        check: &mut impl FnMut() -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        // Migration verifies before replacing its plaintext source; the same verifier
        // also detects a damaged new write before the atomic rename.
        let mut file = std::io::BufReader::new(std::fs::File::open(path)?);
        let mut header = [0u8; 24];
        file.read_exact(&mut header)?;
        if &header[..8] != MAGIC || u64::from_le_bytes(header[16..24].try_into().unwrap()) != total
        {
            return Err(invalid("media verification header mismatch"));
        }
        let prefix: [u8; 8] = header[8..16].try_into().unwrap();
        let cipher = Aes256Gcm::new(self.key.as_bytes().into());
        let mut done = 0u64;
        let mut index = 0u32;
        while done < total || index == 0 {
            check()?;
            let len = ((total - done).min(CHUNK as u64)) as usize;
            let mut ciphertext = vec![0u8; len + 16];
            file.read_exact(&mut ciphertext)?;
            let aad = self.aad(conv, total, index, len as u32);
            let nonce_bytes = nonce(prefix, index);
            let nonce = Nonce::try_from(nonce_bytes.as_slice())
                .map_err(|_| invalid("invalid media nonce"))?;
            cipher
                .decrypt(
                    &nonce,
                    Payload {
                        msg: &ciphertext,
                        aad: &aad,
                    },
                )
                .map_err(|_| invalid("media verification failed"))?;
            done += len as u64;
            index += 1;
        }
        if file.read(&mut [0u8; 1])? != 0 {
            return Err(invalid("media trailing data"));
        }
        Ok(())
    }

    /// Open a production media namespace on the existing participant database.
    pub(in crate::node) fn open_profile(
        profile: Arc<crate::storage::profile::context::ProfileStorage>,
        account_dir: &Path,
        password: &str,
        owner_id: &str,
        entry_limit: Option<usize>,
    ) -> std::io::Result<Self> {
        Self::open_inner(account_dir, password, owner_id, Some(profile), entry_limit)
    }

    fn indexed_inventory(&mut self) -> std::io::Result<()> {
        let index = self.index.as_ref().expect("indexed media").clone();
        if index.imported()? {
            return Ok(());
        }
        for (number, entry) in std::fs::read_dir(&self.dir)?.enumerate() {
            #[cfg(test)]
            directory_observations::visit();
            if self.entry_limit.is_some_and(|limit| number >= limit) {
                return Err(invalid("media directory entry limit"));
            }
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            // A bare `<id>.part` was also a legal completed filename in
            // legacy stores. Preserve that interpretation during import;
            // newly reserved writes carry their exact partial flag in SQL.
            let partial = name
                .strip_suffix(".part")
                .is_some_and(|original| Path::new(original).extension().is_some());
            let original = if partial {
                &name[..name.len() - 5]
            } else {
                name
            };
            let Some(stem) = Path::new(original)
                .file_stem()
                .and_then(|value| value.to_str())
            else {
                continue;
            };
            let mut bytes = [0; 32];
            if stem.len() != 64 || hex::decode_to_slice(stem, &mut bytes).is_err() {
                continue;
            }
            let conv = ConversationId::new(bytes);
            if partial {
                index.register(name, conv, true, Some(false))?;
                continue;
            }
            let mut source = std::fs::File::open(entry.path())?;
            let mut magic = [0; 8];
            let encrypted = source.read(&mut magic)? == 8 && &magic == MAGIC;
            index.register(name, conv, false, Some(!encrypted))?;
            if encrypted {
                continue;
            }
            source.rewind()?;
            let size = source.metadata()?.len();
            if let Err(error) = self.store_stream(conv, &entry.path(), &mut source, size) {
                log::warn!("Media migration failed; original retained: {error}");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(
                        entry.path(),
                        std::fs::Permissions::from_mode(0o600),
                    );
                }
            }
        }
        index.finish_import()
    }

    fn selected_file(&self, file: ConversationId) -> std::io::Result<Option<(PathBuf, bool)>> {
        if let Some(index) = &self.index {
            let mut after = None;
            loop {
                let page = index.exact(file, after.as_deref())?;
                if page.is_empty() {
                    return Ok(None);
                }
                for entry in &page {
                    after = Some(entry.name.clone());
                    if entry.partial {
                        continue;
                    }
                    let path = self.dir.join(&entry.name);
                    match std::fs::symlink_metadata(&path) {
                        Ok(metadata) if metadata.is_file() => {
                            return Ok(Some((path, entry.legacy)));
                        }
                        Ok(_) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
                    }
                }
                if page.len() < 32 {
                    return Ok(None);
                }
            }
        }
        let hex = hex::encode(file.as_bytes());
        for entry in std::fs::read_dir(&self.dir)? {
            #[cfg(test)]
            directory_observations::visit();
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            if path.file_stem().and_then(|stem| stem.to_str()) == Some(hex.as_str()) {
                return Ok(Some((
                    path,
                    self.legacy_unmigrated
                        .lock()
                        .map_err(|_| invalid("media state lock poisoned"))?
                        .contains(&file),
                )));
            }
        }
        Ok(None)
    }
    fn plaintext_source(path: &Path, eligible: bool) -> std::io::Result<bool> {
        if !eligible {
            return Ok(false);
        }
        let mut file = std::fs::File::open(path)?;
        let mut magic = [0; 8];
        Ok(file.read(&mut magic)? != 8 || &magic != MAGIC)
    }
    fn indexed_name<'a>(&self, path: &'a Path) -> std::io::Result<&'a str> {
        if path.parent() != Some(self.dir.as_path()) {
            return Err(invalid("media locator is outside its directory"));
        }
        path.file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| invalid("invalid media locator name"))
    }
    /// Visit a stable inventory with only one fixed metadata page retained.
    pub(in crate::node) fn managed_files(
        &self,
    ) -> std::io::Result<Box<dyn Iterator<Item = std::io::Result<(ConversationId, PathBuf)>>>> {
        let Some(index) = &self.index else {
            return Ok(Box::new(self.legacy_managed_files()?.into_iter().map(Ok)));
        };
        let index = index.clone();
        let dir = self.dir.clone();
        let revision = index.revision()?;
        let mut after: Option<String> = None;
        let mut previous = None;
        let mut pending = std::collections::VecDeque::new();
        let mut done = false;
        Ok(Box::new(std::iter::from_fn(move || {
            loop {
                if done {
                    return None;
                }
                if pending.is_empty() {
                    match index.page(after.as_deref(), Some(revision)) {
                        Ok(page) if page.is_empty() => {
                            done = true;
                            return None;
                        }
                        Ok(page) => pending.extend(page),
                        Err(error) => {
                            done = true;
                            return Some(Err(error));
                        }
                    }
                }
                let entry = pending.pop_front().expect("nonempty media page");
                after = Some(entry.name.clone());
                if entry.partial {
                    continue;
                }
                let path = dir.join(&entry.name);
                match std::fs::symlink_metadata(&path) {
                    Ok(metadata) if metadata.is_file() => {}
                    Ok(_) => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        done = true;
                        return Some(Err(error));
                    }
                }
                if previous == Some(entry.file) {
                    done = true;
                    return Some(Err(invalid("duplicate managed media ID")));
                }
                previous = Some(entry.file);
                return Some(Ok((entry.file, path)));
            }
        })))
    }
    pub(in crate::node) fn schedule_gc(&self) -> std::io::Result<()> {
        match &self.index {
            Some(index) => index.schedule_gc(),
            None => Err(invalid("background media maintenance requires a profile")),
        }
    }
    pub(in crate::node) fn gc_pending(&self) -> std::io::Result<bool> {
        self.index
            .as_ref()
            .map_or(Ok(false), |index| index.gc_pending())
    }
    /// The caller holds its card/reference owner lock for this entire turn, so
    /// an existing file cannot gain a new reference between the check and unlink.
    /// The DB connection is released before invoking that owner's predicate.
    pub(in crate::node) fn gc_step(
        &self,
        mut live: impl FnMut(ConversationId) -> std::io::Result<bool>,
    ) -> std::io::Result<()> {
        let Some(index) = &self.index else {
            return Ok(());
        };
        let page = index.gc_page()?;
        if page.is_empty() {
            return index.advance_gc(None);
        }
        for entry in page {
            let _guard = self
                .mutation
                .lock()
                .map_err(|_| invalid("media mutation lock poisoned"))?;
            if index.gc_eligible(&entry)? {
                let path = self.dir.join(&entry.name);
                if entry.partial || !live(entry.file)? {
                    match std::fs::symlink_metadata(&path) {
                        Ok(metadata) if metadata.is_file() => {
                            std::fs::remove_file(&path)?;
                            sync_parent(&path)?;
                        }
                        Ok(_) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
                    }
                    index.remove(&entry.name, Some(entry.revision))?;
                } else if entry.legacy {
                    let retry = || -> std::io::Result<()> {
                        if !std::fs::symlink_metadata(&path)?.is_file() {
                            return Err(invalid("legacy media is not a regular file"));
                        }
                        let mut source = std::fs::File::open(&path)?;
                        let mut magic = [0; 8];
                        if source.read(&mut magic)? == 8 && &magic == MAGIC {
                            return index.complete(&entry.name);
                        }
                        source.rewind()?;
                        let size = source.metadata()?.len();
                        self.store_stream_locked(entry.file, &path, &mut source, size)
                    };
                    if let Err(error) = retry() {
                        // A persistent filesystem failure must not starve the
                        // remaining inventory. The exact legacy flag survives
                        // and the next scheduled pass retries this row.
                        log::warn!("Media migration retry failed; original retained: {error}");
                    }
                }
            }
            index.advance_gc(Some(&entry.name))?;
        }
        Ok(())
    }

    fn migrate_plaintext(&mut self) -> std::io::Result<()> {
        for entry in std::fs::read_dir(&self.dir)? {
            #[cfg(test)]
            directory_observations::visit();
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(conv_bytes) = hex::decode(stem) else {
                continue;
            };
            let Ok(conv_bytes): Result<[u8; 32], _> = conv_bytes.try_into() else {
                continue;
            };
            let conv = ConversationId::new(conv_bytes);
            let mut source = std::fs::File::open(&path)?;
            let mut magic = [0u8; 8];
            if source.read(&mut magic)? == 8 && &magic == MAGIC {
                continue;
            }
            source.rewind()?;
            let size = source.metadata()?.len();
            if let Err(error) = self.store_stream(conv, &path, &mut source, size) {
                log::warn!("Media migration failed; original retained: {error}");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
                }
                self.legacy_unmigrated.lock().unwrap().insert(conv);
            }
        }
        Ok(())
    }
}

impl super::Node {
    pub(in crate::node) fn media_maintenance_pending(
        &self,
    ) -> Result<bool, crate::eventlog::LogError> {
        Ok(self
            .media
            .gc_pending()
            .map_err(crate::eventlog::LogError::Io)?
            || self
                .favorite_media
                .gc_pending()
                .map_err(crate::eventlog::LogError::Io)?
            || self
                .account_documents
                .lock()
                .expect("account documents lock")
                .media_maintenance_pending()?)
    }
    pub(in crate::node) fn maintain_media(&self) -> Result<(), crate::eventlog::LogError> {
        // Hold each reference owner's lock before the media mutation lock.
        // No new card can reuse bytes between the negative check and unlink.
        {
            let files = self.received_files.lock().expect("received files lock");
            let limit = super::transcript_repository::TranscriptPageLimit::new(1, 4096)?;
            self.media
                .gc_step(|file| {
                    files
                        .file_headers(&file, None, limit)
                        .map(|page| !page.rows.is_empty())
                        .map_err(std::io::Error::other)
                })
                .map_err(crate::eventlog::LogError::Io)?;
        }
        {
            let favorites = self.favorites.lock().expect("favorites lock");
            let now = self.expiry_now();
            self.favorite_media
                .gc_step(|file| Ok(favorites.has_live_file(file, now)))
                .map_err(crate::eventlog::LogError::Io)?;
        }
        self.account_documents
            .lock()
            .expect("account documents lock")
            .maintain_media()
    }
}

fn nonce(prefix: [u8; 8], index: u32) -> [u8; 12] {
    let mut bytes = [0u8; 12];
    bytes[..8].copy_from_slice(&prefix);
    bytes[8..].copy_from_slice(&index.to_le_bytes());
    bytes
}

fn invalid(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn set_private_dir(path: &Path) -> std::io::Result<()> {
    if !std::fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(invalid("media directory is not a regular directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn private_create(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn private_create_new(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

pub(in crate::node) fn sync_parent(path: &Path) -> std::io::Result<()> {
    #[cfg(not(unix))]
    let _ = path;
    #[cfg(unix)]
    std::fs::File::open(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .sync_all()?;
    Ok(())
}

/// Upper bound on a whole-file in-memory read for inline preview (OOM guard). Comfortably
/// above the frontend's inline caps (16 MB image / 50 MB video); larger media uses the
/// streaming save path instead.
const MAX_INLINE_MEDIA_BYTES: u64 = 64 * 1024 * 1024;

/// `dest` + `.part`: the temp a store writes into before its atomic rename.
fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

#[cfg(test)]
mod directory_observations {
    use std::cell::Cell;
    thread_local! { static ENTRIES:Cell<usize>=const{Cell::new(0)}; }
    pub(super) fn visit() {
        ENTRIES.with(|count| count.set(count.get() + 1));
    }
    pub(super) fn reset() {
        ENTRIES.with(|count| count.set(0));
    }
    pub(super) fn entries() -> usize {
        ENTRIES.with(Cell::get)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indexed_profile(directory: &Path) -> Arc<crate::storage::profile::context::ProfileStorage> {
        use crate::{
            identity::{account::Account, device::DeviceIdentity},
            storage::profile::{ProfileStorageLease, context::ProfileStorage},
        };
        let device = DeviceIdentity::from_secret_bytes([21; 32], [22; 32]);
        let account = Account::from_secret_bytes([23; 32]);
        let profile = ProfileStorage::open_participant(
            ProfileStorageLease::acquire(directory).unwrap(),
            &device.public(),
            &account.public(),
            &directory.join("media-test-events.log"),
            &directory.join("media-test-sent.log"),
            "password",
        )
        .unwrap();
        profile.complete_participant("password", None).unwrap();
        profile
    }
    fn indexed_store(directory: &Path) -> MediaStore {
        MediaStore::open_profile(
            indexed_profile(directory),
            directory,
            "password",
            "owner",
            None,
        )
        .unwrap()
    }
    fn indexed_id(number: u64) -> ConversationId {
        let mut id = [0; 32];
        id[..8].copy_from_slice(&number.to_be_bytes());
        ConversationId::new(id)
    }

    #[test]
    fn indexed_media_warm_open_does_not_scan_retained_directory_entries() {
        let directory = tempfile::tempdir().unwrap();
        {
            let store = indexed_store(directory.path());
            for number in 0..512 {
                store
                    .store_bytes(indexed_id(number), "image.png", b"media")
                    .unwrap();
            }
        }
        directory_observations::reset();
        let store = indexed_store(directory.path());
        assert_eq!(
            directory_observations::entries(),
            0,
            "warm media open must load index metadata without walking historical files"
        );
        assert_eq!(store.read(indexed_id(511)).unwrap(), b"media");
    }

    #[test]
    fn indexed_media_selected_path_does_not_scan_other_entries() {
        let directory = tempfile::tempdir().unwrap();
        let store = indexed_store(directory.path());
        for number in 0..512 {
            store
                .store_bytes(indexed_id(number), "image.png", b"media")
                .unwrap();
        }
        directory_observations::reset();
        assert!(store.path(indexed_id(511)).unwrap().is_file());
        assert_eq!(
            directory_observations::entries(),
            0,
            "selected media lookup must seek its exact managed identity"
        );
    }

    #[test]
    fn indexed_media_node_warm_open_defers_orphan_inventory_work() {
        use crate::{discovery::Roster, identity::device::DeviceIdentity, node::Node};
        use tokio::sync::mpsc;
        let directory = tempfile::tempdir().unwrap();
        let secret = DeviceIdentity::generate().secret_bytes();
        let open = || {
            let (dm, _) = mpsc::unbounded_channel();
            let (channel, _) = mpsc::unbounded_channel();
            let (file, _) = mpsc::unbounded_channel();
            Node::open(
                DeviceIdentity::from_secret_bytes(secret.0, secret.1),
                Arc::new(Mutex::new(Roster::default())),
                dm,
                channel,
                file,
                &directory.path().join("messages.log"),
                &directory.path().join("sent.log"),
                "pw",
            )
            .unwrap()
        };
        let node = open();
        for number in 0..512 {
            node.media
                .store_bytes(indexed_id(number), "image.png", b"orphan")
                .unwrap();
        }
        node.close_profile().unwrap();
        directory_observations::reset();
        let node = open();
        assert_eq!(
            directory_observations::entries(),
            0,
            "actual Node startup must not enumerate media for migration or orphan cleanup"
        );
        node.close_profile().unwrap();
    }

    #[test]
    fn indexed_media_failed_plaintext_migrations_do_not_accumulate_resident_ids() {
        let directory = tempfile::tempdir().unwrap();
        let media = directory.path().join("media");
        std::fs::create_dir(&media).unwrap();
        let protected = directory.path().join("preserve");
        std::fs::write(&protected, b"safe").unwrap();
        for number in 0..256 {
            let path = media.join(format!(
                "{}.png",
                hex::encode(indexed_id(number).as_bytes())
            ));
            std::fs::write(&path, b"legacy plaintext").unwrap();
            std::fs::create_dir(part_path(&path)).unwrap();
        }
        let store = indexed_store(directory.path());
        assert!(store.migration_warning());
        assert_eq!(
            store.legacy_unmigrated.lock().unwrap().len(),
            0,
            "failed migration identities belong to the exact disk index, not a historical RAM set"
        );
        assert_eq!(store.read(indexed_id(255)).unwrap(), b"legacy plaintext");
        assert_eq!(std::fs::read(protected).unwrap(), b"safe");
    }

    #[test]
    fn indexed_media_retries_failed_plaintext_migration_in_a_bounded_maintenance_turn() {
        let directory = tempfile::tempdir().unwrap();
        let media = directory.path().join("media");
        std::fs::create_dir(&media).unwrap();
        let file = indexed_id(1);
        let path = media.join(format!("{}.png", hex::encode(file.as_bytes())));
        std::fs::write(&path, b"legacy plaintext").unwrap();
        std::fs::create_dir(part_path(&path)).unwrap();
        let store = indexed_store(directory.path());
        assert!(store.migration_warning());
        assert_eq!(store.read(file).unwrap(), b"legacy plaintext");
        drop(store);
        std::fs::remove_dir(part_path(&path)).unwrap();
        directory_observations::reset();
        let store = indexed_store(directory.path());
        assert_eq!(directory_observations::entries(), 0);
        store.schedule_gc().unwrap();
        store.gc_step(|_| Ok(true)).unwrap();
        assert!(std::fs::read(path).unwrap().starts_with(MAGIC));
        assert!(!store.migration_warning());
        assert_eq!(store.read(file).unwrap(), b"legacy plaintext");
    }

    #[test]
    fn indexed_media_rejects_corrupt_locator_before_read_or_unlink() {
        let directory = tempfile::tempdir().unwrap();
        let profile = indexed_profile(directory.path());
        let store =
            MediaStore::open_profile(profile.clone(), directory.path(), "password", "owner", None)
                .unwrap();
        let file = indexed_id(6);
        store.store_bytes(file, "image.png", b"retained").unwrap();
        let original = format!("{}.png", hex::encode(file.as_bytes()));
        let external = directory.path().join(&original);
        std::fs::write(&external, b"outside media directory").unwrap();
        let malicious = format!("../{original}");
        profile
            .database()
            .transaction(|c| {
                c.execute(
                    "UPDATE media_files SET name=?1,legacy=1 WHERE name=?2",
                    rusqlite::params![malicious, original],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(
            store.read(file).is_none(),
            "a corrupt inventory locator must not read outside its directory"
        );
        store.schedule_gc().unwrap();
        assert!(store.gc_step(|_| Ok(false)).is_err());
        assert_eq!(std::fs::read(external).unwrap(), b"outside media directory");
    }

    #[test]
    fn indexed_media_gc_is_bounded_resumable_and_excludes_later_publications() {
        let directory = tempfile::tempdir().unwrap();
        let profile = indexed_profile(directory.path());
        let store =
            MediaStore::open_profile(profile.clone(), directory.path(), "password", "owner", None)
                .unwrap();
        for number in 0..130 {
            store
                .store_bytes(indexed_id(number), "image.png", b"retained")
                .unwrap();
        }
        store.schedule_gc().unwrap();
        let late = indexed_id(200);
        store
            .store_bytes(late, "image.png", b"published after cutoff")
            .unwrap();
        directory_observations::reset();
        let mut turns = 0;
        while store.gc_pending().unwrap() {
            let mut examined = 0;
            store
                .gc_step(|file| {
                    examined += 1;
                    Ok(file == indexed_id(12))
                })
                .unwrap();
            assert!(examined <= 32);
            turns += 1;
            assert!(turns < 10);
        }
        assert_eq!(directory_observations::entries(), 0);
        assert_eq!(store.read(indexed_id(12)).unwrap(), b"retained");
        assert_eq!(store.read(late).unwrap(), b"published after cutoff");
        assert!(!store.contains(indexed_id(129)));
        // A new startup cutoff can collect the formerly excluded orphan.
        store.schedule_gc().unwrap();
        drop(store);
        drop(profile);
        let reopened = indexed_store(directory.path());
        while reopened.gc_pending().unwrap() {
            reopened.gc_step(|file| Ok(file == indexed_id(12))).unwrap();
        }
        assert!(reopened.contains(indexed_id(12)));
        assert!(!reopened.contains(late));
    }
    #[test]
    fn indexed_media_failed_completion_flag_never_exposes_ciphertext_as_plaintext() {
        let directory = tempfile::tempdir().unwrap();
        let media = directory.path().join("media");
        std::fs::create_dir(&media).unwrap();
        let id = indexed_id(301);
        let path = media.join(format!("{}.png", hex::encode(id.as_bytes())));
        std::fs::write(&path, b"legacy bytes").unwrap();
        let part = part_path(&path);
        std::fs::create_dir(&part).unwrap();
        let profile = indexed_profile(directory.path());
        let store =
            MediaStore::open_profile(profile.clone(), directory.path(), "password", "owner", None)
                .unwrap();
        assert_eq!(store.read(id).unwrap(), b"legacy bytes");
        std::fs::remove_dir(&part).unwrap();
        profile.database().transaction(|c|{c.execute_batch("CREATE TRIGGER reject_media_flag BEFORE UPDATE OF legacy ON media_files WHEN OLD.legacy=1 AND NEW.legacy=0 BEGIN SELECT RAISE(ABORT,'injected media metadata failure'); END")?;Ok(())}).unwrap();
        assert!(
            store
                .store_bytes(id, "image.png", b"encrypted replacement")
                .is_err()
        );
        assert_ne!(std::fs::read(&path).unwrap(), b"encrypted replacement");
        assert_eq!(
            store.read(id).unwrap(),
            b"encrypted replacement",
            "a successful rename followed by SQL failure must still authenticate/decrypt the encrypted header"
        );
        profile
            .database()
            .transaction(|c| {
                c.execute_batch("DROP TRIGGER reject_media_flag")?;
                Ok(())
            })
            .unwrap();
        store
            .store_bytes(id, "image.png", b"encrypted replacement")
            .unwrap();
        assert!(!store.migration_warning());
    }
    #[test]
    fn indexed_media_gc_storage_failure_preserves_retry_cursor_and_new_reference() {
        let directory = tempfile::tempdir().unwrap();
        let profile = indexed_profile(directory.path());
        let store =
            MediaStore::open_profile(profile.clone(), directory.path(), "password", "owner", None)
                .unwrap();
        let id = indexed_id(302);
        store.store_bytes(id, "image.png", b"old orphan").unwrap();
        store.schedule_gc().unwrap();
        profile.database().transaction(|c|{c.execute_batch("CREATE TRIGGER reject_media_retirement BEFORE DELETE ON media_files BEGIN SELECT RAISE(ABORT,'injected media retirement failure'); END")?;Ok(())}).unwrap();
        assert!(store.gc_step(|_| Ok(false)).is_err());
        assert!(store.gc_pending().unwrap());
        profile
            .database()
            .transaction(|c| {
                c.execute_batch("DROP TRIGGER reject_media_retirement")?;
                Ok(())
            })
            .unwrap();
        store
            .store_bytes(id, "image.png", b"new retained bytes")
            .unwrap();
        while store.gc_pending().unwrap() {
            store.gc_step(|_| Ok(false)).unwrap();
        }
        assert_eq!(
            store.read(id).unwrap(),
            b"new retained bytes",
            "replacement revision fences the old orphan decision"
        );
    }
    #[test]
    fn indexed_media_export_pages_reject_duplicate_ids_and_changed_inventory() {
        let directory = tempfile::tempdir().unwrap();
        let store = indexed_store(directory.path());
        for number in 0..40 {
            store
                .store_bytes(indexed_id(number), "image.png", b"bytes")
                .unwrap();
        }
        let mut export = store.managed_files().unwrap();
        assert!(export.next().unwrap().is_ok());
        store
            .store_bytes(indexed_id(100), "image.png", b"new")
            .unwrap();
        assert!(export.collect::<std::io::Result<Vec<_>>>().is_err());
        store
            .store_bytes(indexed_id(1), "image.jpg", b"same ID")
            .unwrap();
        assert!(
            store
                .managed_files()
                .unwrap()
                .collect::<std::io::Result<Vec<_>>>()
                .is_err()
        );
    }

    #[test]
    fn indexed_media_preserves_completed_part_extension_and_tracks_new_partials() {
        let directory = tempfile::tempdir().unwrap();
        let media = directory.path().join("media");
        std::fs::create_dir(&media).unwrap();
        let old = indexed_id(50);
        std::fs::write(
            media.join(format!("{}.part", hex::encode(old.as_bytes()))),
            b"legacy complete",
        )
        .unwrap();
        let store = indexed_store(directory.path());
        assert_eq!(store.read(old).unwrap(), b"legacy complete");
        let new = indexed_id(51);
        store
            .store_bytes(new, "complete.part", b"new complete")
            .unwrap();
        assert_eq!(store.read(new).unwrap(), b"new complete");
        assert_eq!(store.managed_files().unwrap().count(), 2);
    }

    #[test]
    fn indexed_media_export_groups_legacy_case_variants_by_exact_id() {
        let directory = tempfile::tempdir().unwrap();
        let media = directory.path().join("media");
        std::fs::create_dir(&media).unwrap();
        for name in [
            format!("{}.jpg", "AB".repeat(32)),
            format!("{}.png", "BB".repeat(32)),
            format!("{}.png", "ab".repeat(32)),
        ] {
            std::fs::write(media.join(name), b"legacy bytes").unwrap();
        }
        let store = indexed_store(directory.path());
        assert!(
            store
                .managed_files()
                .unwrap()
                .collect::<std::io::Result<Vec<_>>>()
                .is_err(),
            "legacy spelling cannot split duplicate identities across the export cursor"
        );
    }

    fn conv(n: u8) -> ConversationId {
        ConversationId::new([n; 32])
    }

    #[test]
    fn classifies_media_by_extension() {
        assert!(is_media_name("photo.PNG"));
        assert!(is_media_name("clip.mp4"));
        assert!(is_media_name("shot.jpeg"));
        assert!(!is_media_name("report.pdf"));
        assert!(!is_media_name("archive.zip"));
        assert!(!is_media_name("noext"));
    }

    #[test]
    fn automatic_images_exclude_active_content_and_large_offers() {
        let mut manifest = crate::file::FileManifest {
            name: "photo.png".into(),
            size: 1024,
            mime: "image/png".into(),
            checksum: [0; 32],
            file_key: [1; 32],
            file_conv: conv(1),
            chunk_count: 1,
        };
        assert!(automatic_inline_image(&crate::file::AnyManifest::V1(
            manifest.clone()
        )));
        manifest.size = 17 * 1024 * 1024;
        assert!(!automatic_inline_image(&crate::file::AnyManifest::V1(
            manifest.clone()
        )));
        manifest.size = 1024;
        manifest.name = "vector.svg".into();
        manifest.mime = "image/svg+xml".into();
        assert!(!automatic_inline_image(&crate::file::AnyManifest::V1(
            manifest
        )));
        assert!(!raster_signature_matches(
            "image/png",
            b"MZ renamed executable"
        ));
        assert!(raster_signature_matches("image/png", b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn stores_and_reads_back_by_conv() {
        let dir = tempfile::tempdir().unwrap();
        let store = MediaStore::open(dir.path(), "password", "account").unwrap();
        assert!(!store.contains(conv(1)));
        store.store_bytes(conv(1), "photo.png", b"PNGDATA").unwrap();
        assert!(store.contains(conv(1)));
        assert_eq!(store.read(conv(1)).as_deref(), Some(&b"PNGDATA"[..]));
        // The stored path keeps the original extension so the asset layer can sniff it.
        assert_eq!(store.path(conv(1)).unwrap().extension().unwrap(), "png");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(store.path(conv(1)).unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(dir.path().join("media"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn managed_media_listing_excludes_partial_and_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = MediaStore::open(dir.path(), "password", "account").unwrap();
        let id = conv(4);
        store.store_bytes(id, "photo.png", b"image").unwrap();
        std::fs::write(dir.path().join("media").join("orphan.part"), b"no").unwrap();
        std::fs::write(
            dir.path()
                .join("media")
                .join(format!("{}.png", "z".repeat(64))),
            b"not a managed ID",
        )
        .unwrap();
        let files = store
            .managed_files()
            .unwrap()
            .collect::<std::io::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, id);

        std::fs::write(
            dir.path()
                .join("media")
                .join(format!("{}.jpg", hex::encode(id.as_bytes()))),
            b"ambiguous duplicate",
        )
        .unwrap();
        assert_eq!(
            store
                .managed_files()
                .and_then(|files| files.collect::<std::io::Result<Vec<_>>>())
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn store_from_path_streams_source() {
        let dir = tempfile::tempdir().unwrap();
        let store = MediaStore::open(dir.path(), "password", "account").unwrap();
        let src = dir.path().join("src.jpg");
        std::fs::write(&src, vec![0x42u8; 200_000]).unwrap();
        store.store_from_path(conv(7), "src.jpg", &src).unwrap();
        assert_eq!(store.read(conv(7)).unwrap().len(), 200_000);
    }

    #[test]
    fn stored_bytes_are_encrypted_and_tampering_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = MediaStore::open(dir.path(), "password", "account").unwrap();
        store
            .store_bytes(conv(2), "private.png", b"private media bytes")
            .unwrap();
        let path = store.path(conv(2)).unwrap();
        let mut disk = std::fs::read(&path).unwrap();
        assert!(disk.starts_with(MAGIC));
        assert!(!disk.windows(7).any(|window| window == b"private"));
        *disk.last_mut().unwrap() ^= 1;
        std::fs::write(path, disk).unwrap();
        assert!(store.read(conv(2)).is_none());
    }

    #[test]
    fn migrates_legacy_plaintext_once_and_keeps_it_readable() {
        let dir = tempfile::tempdir().unwrap();
        let media_dir = dir.path().join("media");
        std::fs::create_dir(&media_dir).unwrap();
        let path = media_dir.join(format!("{}.png", hex::encode(conv(3).as_bytes())));
        std::fs::write(&path, b"old plaintext").unwrap();
        let store = MediaStore::open(dir.path(), "password", "account").unwrap();
        assert_eq!(store.read(conv(3)).unwrap(), b"old plaintext");
        assert!(std::fs::read(&path).unwrap().starts_with(MAGIC));
        drop(store);
        let reopened = MediaStore::open(dir.path(), "password", "account").unwrap();
        assert_eq!(reopened.read(conv(3)).unwrap(), b"old plaintext");
    }

    #[test]
    fn empty_media_is_authenticated() {
        let dir = tempfile::tempdir().unwrap();
        let store = MediaStore::open(dir.path(), "password", "account").unwrap();
        store.store_bytes(conv(4), "empty.png", b"").unwrap();
        assert_eq!(store.read(conv(4)).unwrap(), b"");
        let path = store.path(conv(4)).unwrap();
        let mut disk = std::fs::read(&path).unwrap();
        disk[16] = 1;
        std::fs::write(path, disk).unwrap();
        assert!(store.read(conv(4)).is_none());
    }

    #[test]
    fn account_binding_rejects_a_copied_ciphertext() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let first = MediaStore::open(a.path(), "password", "account-a").unwrap();
        MediaStore::open(b.path(), "password", "account-b").unwrap();
        // Use the same salt so this exercises authenticated account binding, not KDF drift.
        std::fs::copy(
            a.path().join("media/key.salt"),
            b.path().join("media/key.salt"),
        )
        .unwrap();
        let second = MediaStore::open(b.path(), "password", "account-b").unwrap();
        first.store_bytes(conv(5), "copy.png", b"secret").unwrap();
        std::fs::copy(
            first.path(conv(5)).unwrap(),
            second.path_for(conv(5), "copy.png"),
        )
        .unwrap();
        assert!(second.read(conv(5)).is_none());
    }

    #[test]
    fn startup_prune_removes_only_unreferenced_media() {
        let dir = tempfile::tempdir().unwrap();
        let store = MediaStore::open(dir.path(), "password", "device").unwrap();
        store.store_bytes(conv(7), "keep.png", b"keep").unwrap();
        store.store_bytes(conv(8), "erase.png", b"erase").unwrap();
        store
            .store_bytes(conv(8), "erase.jpg", b"erase duplicate")
            .unwrap();
        let unknown = store.dir.join("unmanaged.txt");
        std::fs::write(&unknown, b"preserve").unwrap();
        store.legacy_unmigrated.lock().unwrap().insert(conv(8));
        #[cfg(unix)]
        let link = {
            let link = store
                .dir
                .join(format!("{}.png", hex::encode(conv(9).as_bytes())));
            std::os::unix::fs::symlink(&unknown, &link).unwrap();
            link
        };
        store.prune_unreferenced(&HashSet::from([conv(7)])).unwrap();
        assert_eq!(store.read(conv(7)).unwrap(), b"keep");
        assert!(!store.contains(conv(8)));
        assert!(!store.legacy_unmigrated.lock().unwrap().contains(&conv(8)));
        assert_eq!(std::fs::read(unknown).unwrap(), b"preserve");
        #[cfg(unix)]
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn failed_migration_keeps_original_and_reports_warning() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let media_dir = dir.path().join("media");
        std::fs::create_dir(&media_dir).unwrap();
        let path = media_dir.join(format!("{}.png", hex::encode(conv(6).as_bytes())));
        std::fs::write(&path, b"old plaintext").unwrap();
        let other = dir.path().join("do-not-touch");
        std::fs::write(&other, b"safe").unwrap();
        symlink(&other, part_path(&path)).unwrap();
        let store = MediaStore::open(dir.path(), "password", "account").unwrap();
        assert!(store.migration_warning());
        assert_eq!(store.read(conv(6)).unwrap(), b"old plaintext");
        assert_eq!(std::fs::read(other).unwrap(), b"safe");
    }
}
