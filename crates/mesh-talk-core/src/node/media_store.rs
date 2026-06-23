//! The chat-media store: a DURABLE, plaintext copy of image/screenshot/video bytes that
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
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

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
}

impl MediaStore {
    /// Open (creating if needed) the media store under `account_dir`.
    pub fn open(account_dir: &Path) -> std::io::Result<Self> {
        let dir = account_dir.join("media");
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// The on-disk path a `file_conv` of media `name` is stored at: `<dir>/<hex>.<ext>`.
    fn path_for(&self, file_conv: ConversationId, name: &str) -> PathBuf {
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
        let part = part_path(&dest);
        {
            let mut reader = std::io::BufReader::new(std::fs::File::open(src)?);
            let mut writer = std::io::BufWriter::new(std::fs::File::create(&part)?);
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                writer.write_all(&buf[..n])?;
            }
            writer.flush()?;
        }
        std::fs::rename(&part, &dest)?;
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
        let part = part_path(&dest);
        std::fs::write(&part, bytes)?;
        std::fs::rename(&part, &dest)?;
        Ok(dest)
    }

    /// Read the stored media bytes for `file_conv` (used by the `read_media` IPC accessor).
    /// Refuses to load a file larger than [`MAX_INLINE_MEDIA_BYTES`] fully into memory — an
    /// oversized media file (up to the 4 GiB file cap) would OOM the process; the UI gates
    /// previews well under this, so a `None` here just means "no inline preview".
    pub fn read(&self, file_conv: ConversationId) -> Option<Vec<u8>> {
        let path = self.path(file_conv)?;
        let meta = std::fs::metadata(&path).ok()?;
        if meta.len() > MAX_INLINE_MEDIA_BYTES {
            return None;
        }
        std::fs::read(path).ok()
    }
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
    fn stores_and_reads_back_by_conv() {
        let dir = tempfile::tempdir().unwrap();
        let store = MediaStore::open(dir.path()).unwrap();
        assert!(!store.contains(conv(1)));
        store.store_bytes(conv(1), "photo.png", b"PNGDATA").unwrap();
        assert!(store.contains(conv(1)));
        assert_eq!(store.read(conv(1)).as_deref(), Some(&b"PNGDATA"[..]));
        // The stored path keeps the original extension so the asset layer can sniff it.
        assert_eq!(store.path(conv(1)).unwrap().extension().unwrap(), "png");
    }

    #[test]
    fn store_from_path_streams_source() {
        let dir = tempfile::tempdir().unwrap();
        let store = MediaStore::open(dir.path()).unwrap();
        let src = dir.path().join("src.jpg");
        std::fs::write(&src, vec![0x42u8; 200_000]).unwrap();
        store.store_from_path(conv(7), "src.jpg", &src).unwrap();
        assert_eq!(store.read(conv(7)).unwrap().len(), 200_000);
    }
}
