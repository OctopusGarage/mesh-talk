//! File crypto: a per-file symmetric key + raw-key AES-256-GCM per chunk (the same
//! primitive as `channel::crypto`), a whole-file SHA-256 checksum for end-to-end
//! verification, and fixed-size chunking. Pure — no I/O, no network.
//!
//! Two chunk-sealing schemes coexist:
//!
//! * **Legacy (v1)** — [`seal_chunk`] / [`open_chunk`]: `nonce ‖ ciphertext` with a
//!   fresh RANDOM 96-bit nonce per chunk. Small single-blob-era files. Kept so old
//!   manifests still decode; not produced by the large-file path anymore.
//! * **Chunked streaming (v2)** — [`seal_chunk_indexed`] / [`open_chunk_indexed`]:
//!   a DETERMINISTIC nonce derived from a per-file 64-bit random salt and the chunk
//!   index, so the nonce is NOT stored per chunk (the ciphertext is just the AEAD
//!   output) and a specific chunk can be re-derived/re-requested independently. This
//!   is the unit the large-file pipeline streams to/from disk.
//!
//! ## v2 nonce derivation (documented)
//!
//! Each file carries a random 8-byte `file_nonce` salt in its manifest. The 12-byte
//! AES-GCM nonce for chunk `i` is `file_nonce (8 B, big-endian as given) ‖ i (u32
//! big-endian)`. The key is per-file and fresh, so (key, nonce) pairs are unique as
//! long as `i` is unique within the file — which it is (one chunk per index). The
//! 64-bit random salt makes the nonce space collision-resistant across files even if
//! a key were ever reused, but the primary safety rests on the fresh per-file key.

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use rand_core::OsRng;
use rand_core::RngCore;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

const KEY_SIZE: usize = 32;
const NONCE_SIZE: usize = 12;
const TAG_SIZE: usize = 16;

/// The chunk size for splitting a file (48 KiB). Must be smaller than the
/// transport's `MAX_PLAINTEXT` (65,519 B) once AES-GCM overhead is added;
/// 48 KiB leaves ample room for sync framing overhead. The last chunk may be
/// smaller. This is the plaintext size; a 1-byte file is one chunk.
pub const CHUNK_SIZE: usize = 48 * 1024;

/// A per-file symmetric key (AES-256-GCM). Generated fresh per file; carried inside
/// the (sealed) `FileManifest` so only conversation members can decrypt chunks.
#[derive(Clone)]
pub struct FileKey([u8; KEY_SIZE]);

/// Zeroize the per-file key on drop so it does not linger in freed memory (mirrors
/// `GroupKey`/`RatchetState`). Each `Clone` owns its own copy, each zeroized on its
/// own drop. The key bytes still travel inside the sealed manifest unchanged, so this
/// changes no wire/on-disk format.
impl Drop for FileKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl FileKey {
    pub fn generate() -> Self {
        let mut key = [0u8; KEY_SIZE];
        OsRng.fill_bytes(&mut key);
        FileKey(key)
    }
    pub fn from_bytes(bytes: [u8; KEY_SIZE]) -> Self {
        FileKey(bytes)
    }
    pub fn as_bytes(&self) -> &[u8; KEY_SIZE] {
        &self.0
    }
}

/// Errors from file crypto.
#[derive(Debug)]
pub enum FileError {
    /// AES-256-GCM chunk encryption failed.
    Encrypt,
    /// AES-256-GCM chunk decryption failed (wrong key or tampered).
    Decrypt,
    /// A malformed chunk envelope (too short for nonce + tag).
    Malformed(String),
    /// Reassembled plaintext failed its SHA-256 checksum.
    ChecksumMismatch,
    /// A chunk's plaintext failed its per-chunk hash in the manifest.
    ChunkHashMismatch(u32),
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileError::Encrypt => write!(f, "file chunk encrypt error"),
            FileError::Decrypt => write!(f, "file chunk decrypt error"),
            FileError::Malformed(m) => write!(f, "malformed file chunk: {m}"),
            FileError::ChecksumMismatch => write!(f, "file checksum mismatch after reassembly"),
            FileError::ChunkHashMismatch(i) => write!(f, "chunk {i} failed its per-chunk hash"),
        }
    }
}

impl std::error::Error for FileError {}

/// SHA-256 of the whole file plaintext — recorded in the manifest, re-checked after
/// reassembly for end-to-end integrity.
pub fn file_checksum(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// SHA-256 of a single chunk's plaintext — recorded per chunk in the v2 manifest so a
/// received chunk can be verified on arrival (independent of the whole-file checksum).
pub fn chunk_hash(plaintext: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(plaintext);
    hasher.finalize().into()
}

/// A fresh random 8-byte per-file nonce salt for the v2 deterministic-nonce scheme.
pub fn generate_file_nonce() -> [u8; 8] {
    let mut salt = [0u8; 8];
    OsRng.fill_bytes(&mut salt);
    salt
}

/// The 12-byte AES-GCM nonce for chunk `index`: `file_nonce ‖ index` (u32 BE).
fn chunk_nonce(file_nonce: &[u8; 8], index: u32) -> [u8; NONCE_SIZE] {
    let mut nonce = [0u8; NONCE_SIZE];
    nonce[..8].copy_from_slice(file_nonce);
    nonce[8..].copy_from_slice(&index.to_be_bytes());
    nonce
}

/// Split `data` into `CHUNK_SIZE` pieces (the last may be smaller). An empty input
/// yields a single empty chunk so the manifest always has `chunk_count >= 1` and
/// reassembly is symmetric.
pub fn split_chunks(data: &[u8]) -> Vec<&[u8]> {
    if data.is_empty() {
        return vec![&data[0..0]];
    }
    data.chunks(CHUNK_SIZE).collect()
}

/// Number of `CHUNK_SIZE` chunks for a file of `size` bytes (>= 1; empty == 1).
pub fn chunk_count_for(size: u64) -> u32 {
    if size == 0 {
        return 1;
    }
    size.div_ceil(CHUNK_SIZE as u64) as u32
}

// ---- v2: deterministic-nonce, indexed chunk crypto (the large-file path) ----

/// Seal chunk `index` with a deterministic nonce. The output is JUST the AES-GCM
/// ciphertext+tag (no nonce prefix — the receiver re-derives the nonce from
/// `file_nonce` + `index`).
pub fn seal_chunk_indexed(
    key: &FileKey,
    file_nonce: &[u8; 8],
    index: u32,
    plaintext: &[u8],
) -> Result<Vec<u8>, FileError> {
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes()).map_err(|_| FileError::Encrypt)?;
    let nonce = chunk_nonce(file_nonce, index);
    cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| FileError::Encrypt)
}

/// Open a chunk produced by [`seal_chunk_indexed`], re-deriving its nonce from
/// `file_nonce` + `index`.
pub fn open_chunk_indexed(
    key: &FileKey,
    file_nonce: &[u8; 8],
    index: u32,
    ciphertext: &[u8],
) -> Result<Vec<u8>, FileError> {
    if ciphertext.len() < TAG_SIZE {
        return Err(FileError::Malformed("chunk too short (need tag)".into()));
    }
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes()).map_err(|_| FileError::Decrypt)?;
    let nonce = chunk_nonce(file_nonce, index);
    cipher
        .decrypt(Nonce::from_slice(&nonce), ciphertext)
        .map_err(|_| FileError::Decrypt)
}

// ---- v1 (legacy): random-nonce chunk crypto, kept for decoding old files ----

/// Seal one chunk: `nonce ‖ AES-256-GCM(key, nonce, plaintext)`, fresh 96-bit nonce.
/// LEGACY — the large-file path uses [`seal_chunk_indexed`].
pub fn seal_chunk(key: &FileKey, plaintext: &[u8]) -> Result<Vec<u8>, FileError> {
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes()).map_err(|_| FileError::Encrypt)?;
    let mut nonce = [0u8; NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| FileError::Encrypt)?;
    let mut out = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Open a chunk produced by [`seal_chunk`]. LEGACY.
pub fn open_chunk(key: &FileKey, envelope: &[u8]) -> Result<Vec<u8>, FileError> {
    if envelope.len() < NONCE_SIZE + TAG_SIZE {
        return Err(FileError::Malformed(
            "chunk too short (need nonce + tag)".into(),
        ));
    }
    let (nonce, ciphertext) = envelope.split_at(NONCE_SIZE);
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes()).map_err(|_| FileError::Decrypt)?;
    cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| FileError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_round_trips() {
        let key = FileKey::generate();
        let sealed = seal_chunk(&key, b"hello file").unwrap();
        assert_eq!(open_chunk(&key, &sealed).unwrap(), b"hello file");
    }

    #[test]
    fn a_different_key_cannot_open_a_chunk() {
        let key = FileKey::generate();
        let other = FileKey::generate();
        let sealed = seal_chunk(&key, b"secret").unwrap();
        assert!(open_chunk(&other, &sealed).is_err());
    }

    #[test]
    fn a_tampered_chunk_fails() {
        let key = FileKey::generate();
        let mut sealed = seal_chunk(&key, b"secret").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0xFF;
        assert!(open_chunk(&key, &sealed).is_err());
        sealed[last] ^= 0xFF;
        sealed[0] ^= 0xFF; // tamper the nonce too
        assert!(open_chunk(&key, &sealed).is_err());
    }

    #[test]
    fn a_short_envelope_is_malformed() {
        let key = FileKey::generate();
        assert!(matches!(
            open_chunk(&key, &[0u8; 4]),
            Err(FileError::Malformed(_))
        ));
    }

    #[test]
    fn split_chunks_covers_all_bytes_and_handles_empty() {
        assert_eq!(split_chunks(b"").len(), 1);
        assert_eq!(split_chunks(b"").first().unwrap().len(), 0);
        let data = vec![7u8; CHUNK_SIZE * 2 + 5];
        let chunks = split_chunks(&data);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), CHUNK_SIZE);
        assert_eq!(chunks[2].len(), 5);
        let rejoined: Vec<u8> = chunks.concat();
        assert_eq!(rejoined, data);
    }

    #[test]
    fn checksum_is_stable_and_sensitive() {
        assert_eq!(file_checksum(b"abc"), file_checksum(b"abc"));
        assert_ne!(file_checksum(b"abc"), file_checksum(b"abd"));
    }

    // ---- v2 indexed crypto ----

    #[test]
    fn indexed_chunk_round_trips() {
        let key = FileKey::generate();
        let salt = generate_file_nonce();
        let sealed = seal_chunk_indexed(&key, &salt, 0, b"hello").unwrap();
        assert_eq!(
            open_chunk_indexed(&key, &salt, 0, &sealed).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn indexed_chunk_with_wrong_index_fails() {
        let key = FileKey::generate();
        let salt = generate_file_nonce();
        let sealed = seal_chunk_indexed(&key, &salt, 3, b"data").unwrap();
        // Decrypting at the wrong index derives a different nonce → AEAD fails.
        assert!(open_chunk_indexed(&key, &salt, 4, &sealed).is_err());
        assert!(open_chunk_indexed(&key, &salt, 3, &sealed).is_ok());
    }

    #[test]
    fn indexed_distinct_indices_give_distinct_nonces() {
        // Same key + same plaintext at different indices must differ (nonce includes
        // the index), so identical chunks don't reuse a nonce.
        let key = FileKey::generate();
        let salt = generate_file_nonce();
        let a = seal_chunk_indexed(&key, &salt, 0, b"same").unwrap();
        let b = seal_chunk_indexed(&key, &salt, 1, b"same").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn indexed_tampered_chunk_fails() {
        let key = FileKey::generate();
        let salt = generate_file_nonce();
        let mut sealed = seal_chunk_indexed(&key, &salt, 0, b"important").unwrap();
        sealed[0] ^= 0xFF;
        assert!(open_chunk_indexed(&key, &salt, 0, &sealed).is_err());
    }

    #[test]
    fn indexed_short_ciphertext_is_malformed() {
        let key = FileKey::generate();
        let salt = generate_file_nonce();
        assert!(matches!(
            open_chunk_indexed(&key, &salt, 0, &[0u8; 4]),
            Err(FileError::Malformed(_))
        ));
    }

    #[test]
    fn chunk_count_for_handles_boundaries() {
        assert_eq!(chunk_count_for(0), 1);
        assert_eq!(chunk_count_for(1), 1);
        assert_eq!(chunk_count_for(CHUNK_SIZE as u64), 1);
        assert_eq!(chunk_count_for(CHUNK_SIZE as u64 + 1), 2);
        assert_eq!(chunk_count_for(2 * CHUNK_SIZE as u64), 2);
    }

    #[test]
    fn chunk_hash_is_stable_and_sensitive() {
        assert_eq!(chunk_hash(b"abc"), chunk_hash(b"abc"));
        assert_ne!(chunk_hash(b"abc"), chunk_hash(b"abd"));
    }
}
