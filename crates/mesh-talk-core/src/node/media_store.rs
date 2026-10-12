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

/// The durable chat-media store rooted at `<account_dir>/media`. Cheap to clone (just the
/// dir); all state is on disk.
#[derive(Clone)]
pub struct MediaStore {
    dir: PathBuf,
    key: Arc<EncryptionKey>,
    owner_id: String,
    legacy_unmigrated: Arc<Mutex<HashSet<ConversationId>>>,
}

impl MediaStore {
    /// Open (creating if needed) the media store under `account_dir`.
    pub fn open(account_dir: &Path, password: &str, owner_id: &str) -> std::io::Result<Self> {
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
        let mut store = Self {
            dir,
            key: Arc::new(key),
            owner_id: owner_id.into(),
            legacy_unmigrated: Arc::new(Mutex::new(HashSet::new())),
        };
        store.migrate_plaintext()?;
        Ok(store)
    }

    /// Enumerate only regular files named for a managed file conversation.
    /// Symlinks and incomplete `.part` writes are never backup sources.
    pub(in crate::node) fn managed_files(&self) -> std::io::Result<Vec<(ConversationId, PathBuf)>> {
        let mut files = std::collections::BTreeMap::new();
        for entry in std::fs::read_dir(&self.dir)? {
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
        let hex = hex::encode(file_conv.as_bytes());
        let entries = std::fs::read_dir(&self.dir).ok()?;
        for entry in entries.flatten() {
            let p = entry.path();
            if !entry.file_type().ok()?.is_file() {
                continue;
            }
            let stem = p.file_stem().and_then(|s| s.to_str());
            if stem == Some(hex.as_str()) {
                return Some(p);
            }
        }
        None
    }

    /// Whether media bytes for `file_conv` are durably stored.
    pub fn contains(&self, file_conv: ConversationId) -> bool {
        self.path(file_conv).is_some()
    }

    pub fn migration_warning(&self) -> bool {
        self.legacy_unmigrated
            .lock()
            .is_ok_and(|items| !items.is_empty())
    }

    pub fn remove(&self, file_conv: ConversationId) -> std::io::Result<()> {
        let hex = hex::encode(file_conv.as_bytes());
        let mut removed_path = None;
        for entry in std::fs::read_dir(&self.dir)? {
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
        for entry in std::fs::read_dir(&self.dir)? {
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
            if !live.contains(&conv) {
                self.remove(conv)?;
            }
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
        let path = self.path(file_conv)?;
        let legacy = self.legacy_unmigrated.lock().ok()?.contains(&file_conv);
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
        let path = self
            .path(file_conv)
            .ok_or_else(|| invalid("media missing"))?;
        if self
            .legacy_unmigrated
            .lock()
            .is_ok_and(|items| items.contains(&file_conv))
        {
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
        let part = part_path(dest);
        let mut file = private_create(&part)?;
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
        self.verify_file(&part, conv, total)?;
        std::fs::rename(&part, dest)?;
        sync_parent(dest)?;
        if let Ok(mut legacy) = self.legacy_unmigrated.lock() {
            legacy.remove(&conv);
        }
        Ok(())
    }

    fn verify_file(&self, path: &Path, conv: ConversationId, total: u64) -> std::io::Result<()> {
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

    fn migrate_plaintext(&mut self) -> std::io::Result<()> {
        for entry in std::fs::read_dir(&self.dir)? {
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
mod tests {
    use super::*;

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
        let store = MediaStore::open(dir.path()).unwrap();
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
        let files = store.managed_files().unwrap();
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
            store.managed_files().unwrap_err().kind(),
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
        store.prune_unreferenced(&HashSet::from([conv(7)])).unwrap();
        assert_eq!(store.read(conv(7)).unwrap(), b"keep");
        assert!(!store.contains(conv(8)));
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
