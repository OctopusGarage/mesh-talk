//! The file manifest: the plaintext announcement of a file (sealed by the node layer
//! with the conversation crypto). Carries the per-file key + the id of the per-file
//! conversation whose events are the chunk ciphertexts, plus integrity hashes.
//!
//! Two wire formats coexist (Phase 2 multi-version pattern):
//!
//! * **v1 [`FileManifest`]** — the original single-checksum, random-nonce format
//!   (positional bincode, no version field). Decode-only now: old peers / old history
//!   still parse; the large-file path no longer produces it.
//! * **v2 [`FileManifestV2`]** — magic+version framed (`MFM2`), adds `chunk_size`, a
//!   per-file nonce salt, and per-chunk SHA-256 hashes so chunks verify on arrival and
//!   resume independently. This is what `send_file_*` writes today.
//!
//! [`decode_manifest`] sniffs the framing and returns whichever it can, failing
//! gracefully (no panic) on garbage or an unknown version.

use crate::eventlog::event::ConversationId;
use crate::file::crypto::{
    chunk_hash, file_checksum, open_chunk, open_chunk_indexed, FileError, FileKey,
};
use bincode::Options;
use serde::{Deserialize, Serialize};

/// Magic + version tag for the v2 manifest framing.
const MFM2_MAGIC: &[u8; 4] = b"MFM2";

/// The original (v1) file announcement. LEGACY: decoded for backward compat, not
/// produced by the large-file path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileManifest {
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub checksum: [u8; 32],
    pub file_key: [u8; 32],
    pub file_conv: ConversationId,
    pub chunk_count: u32,
}

impl FileManifest {
    /// Serialize for an event payload (fixint, matching the codebase wire style).
    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("file manifest serializes")
    }

    /// Parse a v1 manifest, fail-closed (reject trailing bytes).
    ///
    /// This is a DESCRIPTIVE type. It is bincode (positional, no version field), so
    /// it cannot grow a field in place without shifting every following byte and
    /// breaking deployed peers — evolution is via a VERSION BUMP (see
    /// [`FileManifestV2`]), NOT by tolerating trailing bytes. Trailing bytes stay
    /// rejected purely to fail closed on corruption, not for forward-compat.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }

    /// The per-file key as a [`FileKey`].
    pub fn key(&self) -> FileKey {
        FileKey::from_bytes(self.file_key)
    }
}

/// The v2 file announcement: chunked, per-chunk-hashed, deterministic-nonce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileManifestV2 {
    pub name: String,
    pub size: u64,
    pub mime: String,
    /// Whole-file SHA-256 (end-to-end integrity after assembly).
    pub checksum: [u8; 32],
    /// Per-file AES-256-GCM key.
    pub file_key: [u8; 32],
    /// Per-file nonce salt; chunk `i`'s nonce is `file_nonce ‖ i` (see crypto.rs).
    pub file_nonce: [u8; 8],
    /// The per-file conversation holding the chunk events.
    pub file_conv: ConversationId,
    /// Plaintext bytes per chunk (the last chunk may be smaller).
    pub chunk_size: u32,
    /// Number of chunks (== events in `file_conv`); always >= 1.
    pub chunk_count: u32,
    /// SHA-256 of each chunk's PLAINTEXT, in order, for on-arrival verification.
    pub chunk_hashes: Vec<[u8; 32]>,
}

impl FileManifestV2 {
    /// Serialize with the `MFM2` magic+version frame.
    pub fn encode(&self) -> Vec<u8> {
        let body = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("file manifest v2 serializes");
        let mut out = Vec::with_capacity(MFM2_MAGIC.len() + body.len());
        out.extend_from_slice(MFM2_MAGIC);
        out.extend_from_slice(&body);
        out
    }

    /// Parse a `MFM2`-framed manifest, fail-closed (reject trailing bytes / wrong magic).
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let body = bytes.strip_prefix(MFM2_MAGIC.as_slice())?;
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(body)
            .ok()
    }

    pub fn key(&self) -> FileKey {
        FileKey::from_bytes(self.file_key)
    }
}

/// Magic + version tag for the v3 manifest framing.
const MFM3_MAGIC: &[u8; 4] = b"MFM3";

/// The SENDER'S INTENT for a file, set by which button sent it — so media-vs-attachment
/// follows the interaction, not the file extension (a `.mov` sent via the attach button is
/// a `File`, not `Media`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileKind {
    /// Sent via the photo/video button: inline preview + persisted to the chat-media store.
    Media,
    /// Sent via the attach button: a generic attachment that lands in the "received files"
    /// tray (whatever its format).
    File,
}

/// The v3 file announcement: v2 PLUS the sender's `kind`. Embeds [`FileManifestV2`] so all
/// its fields + integrity guarantees carry over unchanged; only the intent is added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileManifestV3 {
    pub v2: FileManifestV2,
    pub kind: FileKind,
}

impl FileManifestV3 {
    /// Serialize with the `MFM3` magic+version frame.
    pub fn encode(&self) -> Vec<u8> {
        let body = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("file manifest v3 serializes");
        let mut out = Vec::with_capacity(MFM3_MAGIC.len() + body.len());
        out.extend_from_slice(MFM3_MAGIC);
        out.extend_from_slice(&body);
        out
    }

    /// Parse an `MFM3`-framed manifest, fail-closed (reject trailing bytes / wrong magic).
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let body = bytes.strip_prefix(MFM3_MAGIC.as_slice())?;
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(body)
            .ok()
    }

    pub fn key(&self) -> FileKey {
        self.v2.key()
    }
}

/// A decoded manifest of any version. The node layer matches on this so v1/v2 history
/// still opens while new files use v3 (which carries the sender's intent).
#[derive(Debug, Clone)]
pub enum AnyManifest {
    V1(FileManifest),
    V2(FileManifestV2),
    V3(FileManifestV3),
}

/// Decode a manifest of either version, sniffing the `MFM2` magic. Fails gracefully
/// (returns `None`) on garbage rather than panicking — old single-blob formats that
/// don't parse simply don't interoperate.
pub fn decode_manifest(bytes: &[u8]) -> Option<AnyManifest> {
    if let Some(v3) = FileManifestV3::decode(bytes) {
        return Some(AnyManifest::V3(v3));
    }
    if let Some(v2) = FileManifestV2::decode(bytes) {
        return Some(AnyManifest::V2(v2));
    }
    FileManifest::decode(bytes).map(AnyManifest::V1)
}

impl AnyManifest {
    pub fn name(&self) -> &str {
        match self {
            AnyManifest::V1(m) => &m.name,
            AnyManifest::V2(m) => &m.name,
            AnyManifest::V3(m) => &m.v2.name,
        }
    }
    pub fn size(&self) -> u64 {
        match self {
            AnyManifest::V1(m) => m.size,
            AnyManifest::V2(m) => m.size,
            AnyManifest::V3(m) => m.v2.size,
        }
    }
    pub fn mime(&self) -> &str {
        match self {
            AnyManifest::V1(m) => &m.mime,
            AnyManifest::V2(m) => &m.mime,
            AnyManifest::V3(m) => &m.v2.mime,
        }
    }
    pub fn file_conv(&self) -> ConversationId {
        match self {
            AnyManifest::V1(m) => m.file_conv,
            AnyManifest::V2(m) => m.file_conv,
            AnyManifest::V3(m) => m.v2.file_conv,
        }
    }
    pub fn chunk_count(&self) -> u32 {
        match self {
            AnyManifest::V1(m) => m.chunk_count,
            AnyManifest::V2(m) => m.chunk_count,
            AnyManifest::V3(m) => m.v2.chunk_count,
        }
    }
    pub fn checksum(&self) -> [u8; 32] {
        match self {
            AnyManifest::V1(m) => m.checksum,
            AnyManifest::V2(m) => m.checksum,
            AnyManifest::V3(m) => m.v2.checksum,
        }
    }
    /// The sender's intent, if the manifest carries it (v3+). `None` for legacy v1/v2
    /// manifests — callers fall back to a filename heuristic for those.
    pub fn kind(&self) -> Option<FileKind> {
        match self {
            AnyManifest::V1(_) | AnyManifest::V2(_) => None,
            AnyManifest::V3(m) => Some(m.kind),
        }
    }
}

/// Open a single chunk (`index` into `file_conv`) against whichever manifest version,
/// verifying the v2 per-chunk hash. Returns the plaintext. Used by the streaming
/// assembler so it never buffers the whole file.
pub fn open_chunk_for(
    manifest: &AnyManifest,
    index: u32,
    ciphertext: &[u8],
) -> Result<Vec<u8>, FileError> {
    match manifest {
        AnyManifest::V1(m) => open_chunk(&m.key(), ciphertext),
        AnyManifest::V2(m) => open_chunk_v2(m, index, ciphertext),
        AnyManifest::V3(m) => open_chunk_v2(&m.v2, index, ciphertext),
    }
}

/// Open + verify a v2-style chunk (shared by the v2 and v3 manifest paths).
fn open_chunk_v2(m: &FileManifestV2, index: u32, ciphertext: &[u8]) -> Result<Vec<u8>, FileError> {
    let plaintext = open_chunk_indexed(&m.key(), &m.file_nonce, index, ciphertext)?;
    let expected = m
        .chunk_hashes
        .get(index as usize)
        .ok_or(FileError::ChunkHashMismatch(index))?;
    if &chunk_hash(&plaintext) != expected {
        return Err(FileError::ChunkHashMismatch(index));
    }
    Ok(plaintext)
}

/// Decrypt `chunks` (the per-file conversation's chunk ciphertexts, in order),
/// concatenate, and verify the whole-file SHA-256. LEGACY in-memory path retained for
/// v1; the v2 path streams via [`open_chunk_for`] instead of buffering.
pub fn reassemble_and_verify(
    manifest: &FileManifest,
    chunks: &[Vec<u8>],
) -> Result<Vec<u8>, FileError> {
    let key = manifest.key();
    // Pre-size from the chunks we actually hold, NOT the manifest's attacker-controlled `size`
    // u64 (which could request a huge allocation). Each chunk decrypts to <= CHUNK_SIZE.
    let cap = chunks.len().saturating_mul(crate::file::crypto::CHUNK_SIZE);
    let mut out: Vec<u8> = Vec::with_capacity(cap);
    for chunk in chunks {
        out.extend_from_slice(&open_chunk(&key, chunk)?);
    }
    if file_checksum(&out) != manifest.checksum {
        return Err(FileError::ChecksumMismatch);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::crypto::{
        chunk_count_for, generate_file_nonce, seal_chunk, seal_chunk_indexed, split_chunks,
        FileKey, CHUNK_SIZE,
    };
    use rand_core::OsRng;
    use rand_core::RngCore;

    fn random_conv() -> ConversationId {
        let mut id = [0u8; 32];
        OsRng.fill_bytes(&mut id);
        ConversationId::new(id)
    }

    fn seal_file_v1(data: &[u8]) -> (FileManifest, Vec<Vec<u8>>) {
        let key = FileKey::generate();
        let chunks: Vec<Vec<u8>> = split_chunks(data)
            .iter()
            .map(|c| seal_chunk(&key, c).unwrap())
            .collect();
        let manifest = FileManifest {
            name: "report.pdf".into(),
            size: data.len() as u64,
            mime: "application/pdf".into(),
            checksum: file_checksum(data),
            file_key: *key.as_bytes(),
            file_conv: random_conv(),
            chunk_count: chunks.len() as u32,
        };
        (manifest, chunks)
    }

    fn seal_file_v2(data: &[u8]) -> (FileManifestV2, Vec<Vec<u8>>) {
        let key = FileKey::generate();
        let file_nonce = generate_file_nonce();
        let plain_chunks = split_chunks(data);
        let chunk_hashes: Vec<[u8; 32]> = plain_chunks.iter().map(|c| chunk_hash(c)).collect();
        let chunks: Vec<Vec<u8>> = plain_chunks
            .iter()
            .enumerate()
            .map(|(i, c)| seal_chunk_indexed(&key, &file_nonce, i as u32, c).unwrap())
            .collect();
        let manifest = FileManifestV2 {
            name: "report.pdf".into(),
            size: data.len() as u64,
            mime: "application/pdf".into(),
            checksum: file_checksum(data),
            file_key: *key.as_bytes(),
            file_nonce,
            file_conv: random_conv(),
            chunk_size: CHUNK_SIZE as u32,
            chunk_count: chunks.len() as u32,
            chunk_hashes,
        };
        (manifest, chunks)
    }

    #[test]
    fn manifest_encodes_to_golden_bytes() {
        // Pins the v1 FileManifest wire layout (fixint bincode). A field reorder or
        // type change breaks interop with already-deployed peers — fail loudly here.
        let m = FileManifest {
            name: "f.txt".into(),
            size: 5,
            mime: "text/plain".into(),
            checksum: [1u8; 32],
            file_key: [2u8; 32],
            file_conv: ConversationId::new([3u8; 32]),
            chunk_count: 1,
        };
        let golden = "0500000000000000662e747874\
0500000000000000\
0a00000000000000746578742f706c61696e\
0101010101010101010101010101010101010101010101010101010101010101\
0202020202020202020202020202020202020202020202020202020202020202\
0303030303030303030303030303030303030303030303030303030303030303\
01000000";
        assert_eq!(hex::encode(m.encode()), golden);
        assert_eq!(FileManifest::decode(&m.encode()), Some(m));
    }

    #[test]
    fn v2_manifest_round_trips_and_rejects_trailing() {
        let (m, _) = seal_file_v2(b"hello");
        let bytes = m.encode();
        assert_eq!(&bytes[..4], MFM2_MAGIC);
        assert_eq!(FileManifestV2::decode(&bytes), Some(m.clone()));
        let mut junk = bytes;
        junk.push(0xAB);
        assert_eq!(FileManifestV2::decode(&junk), None);
    }

    #[test]
    fn decode_manifest_sniffs_version() {
        let (v1, _) = seal_file_v1(b"old");
        let (v2, _) = seal_file_v2(b"new");
        assert!(matches!(
            decode_manifest(&v1.encode()),
            Some(AnyManifest::V1(_))
        ));
        assert!(matches!(
            decode_manifest(&v2.encode()),
            Some(AnyManifest::V2(_))
        ));
        // Garbage fails gracefully (no panic).
        assert!(decode_manifest(b"not a manifest at all").is_none());
        assert!(decode_manifest(&[]).is_none());
    }

    #[test]
    fn v3_manifest_round_trips_and_carries_kind() {
        let (v2, _) = seal_file_v2(b"clip");
        let media = FileManifestV3 {
            v2: v2.clone(),
            kind: FileKind::Media,
        };
        let bytes = media.encode();
        assert_eq!(&bytes[..4], MFM3_MAGIC);
        assert_eq!(FileManifestV3::decode(&bytes), Some(media.clone()));
        // decode_manifest sniffs V3 (not mistaken for V2) and exposes the intent.
        let any = decode_manifest(&bytes).unwrap();
        assert!(matches!(any, AnyManifest::V3(_)));
        assert_eq!(any.kind(), Some(FileKind::Media));
        // The File variant survives too; legacy v1/v2 report no kind.
        let attach = FileManifestV3 {
            v2,
            kind: FileKind::File,
        };
        assert_eq!(
            decode_manifest(&attach.encode()).unwrap().kind(),
            Some(FileKind::File)
        );
        assert_eq!(
            decode_manifest(&seal_file_v2(b"x").0.encode())
                .unwrap()
                .kind(),
            None
        );
        // Trailing junk is rejected.
        let mut junk = bytes;
        junk.push(0x00);
        assert_eq!(FileManifestV3::decode(&junk), None);
    }

    #[test]
    fn v2_streams_a_multi_chunk_file_and_verifies_each_chunk() {
        let data = vec![42u8; CHUNK_SIZE * 2 + 17];
        let (m, chunks) = seal_file_v2(&data);
        assert_eq!(m.chunk_count, chunk_count_for(data.len() as u64));
        assert_eq!(m.chunk_count, 3);
        let any = AnyManifest::V2(m);
        // Stream chunk-by-chunk (the assembler's per-chunk path) and rebuild.
        let mut out = Vec::new();
        for (i, ct) in chunks.iter().enumerate() {
            out.extend_from_slice(&open_chunk_for(&any, i as u32, ct).unwrap());
        }
        assert_eq!(out, data);
        assert_eq!(
            file_checksum(&out),
            match &any {
                AnyManifest::V2(m) => m.checksum,
                _ => unreachable!(),
            }
        );
    }

    #[test]
    fn v2_one_chunk_and_empty_boundary() {
        for data in [vec![], vec![9u8; 1], vec![9u8; CHUNK_SIZE]] {
            let (m, chunks) = seal_file_v2(&data);
            assert_eq!(m.chunk_count, 1);
            let any = AnyManifest::V2(m);
            let out = open_chunk_for(&any, 0, &chunks[0]).unwrap();
            assert_eq!(out, data);
        }
    }

    #[test]
    fn v2_exact_two_chunk_boundary() {
        let data = vec![3u8; CHUNK_SIZE * 2];
        let (m, _) = seal_file_v2(&data);
        assert_eq!(m.chunk_count, 2);
    }

    #[test]
    fn v2_tampered_chunk_fails_per_chunk_hash_or_aead() {
        let (m, mut chunks) = seal_file_v2(b"important data");
        chunks[0][0] ^= 0xFF;
        let any = AnyManifest::V2(m);
        // AEAD catches it (or, if it somehow opened, the per-chunk hash would).
        assert!(open_chunk_for(&any, 0, &chunks[0]).is_err());
    }

    #[test]
    fn v2_chunk_at_wrong_index_is_rejected() {
        // A chunk delivered/decrypted at the wrong index fails: nonce mismatch (AEAD)
        // and/or per-chunk hash mismatch. This is what makes a specific chunk
        // independently verifiable for resume.
        let data = {
            let mut d = vec![0u8; CHUNK_SIZE];
            d.resize(2 * CHUNK_SIZE, 1u8);
            d
        };
        let (m, chunks) = seal_file_v2(&data);
        let any = AnyManifest::V2(m);
        // chunk 1's ciphertext opened as index 0 → wrong nonce → fails.
        assert!(open_chunk_for(&any, 0, &chunks[1]).is_err());
    }

    #[test]
    fn v2_missing_hash_entry_is_rejected() {
        let (mut m, chunks) = seal_file_v2(b"data");
        m.chunk_hashes.clear(); // attacker drops the hash list
        let any = AnyManifest::V2(m);
        assert!(matches!(
            open_chunk_for(&any, 0, &chunks[0]),
            Err(FileError::ChunkHashMismatch(0))
        ));
    }

    // ---- v1 legacy reassembly (unchanged behaviour) ----

    #[test]
    fn v1_reassembles_a_multi_chunk_file_and_verifies() {
        let data = vec![42u8; CHUNK_SIZE * 2 + 17];
        let (m, chunks) = seal_file_v1(&data);
        assert_eq!(m.chunk_count, 3);
        assert_eq!(reassemble_and_verify(&m, &chunks).unwrap(), data);
    }

    #[test]
    fn v1_empty_file_reassembles() {
        let (m, chunks) = seal_file_v1(b"");
        assert_eq!(m.chunk_count, 1);
        assert_eq!(
            reassemble_and_verify(&m, &chunks).unwrap(),
            Vec::<u8>::new()
        );
    }

    #[test]
    fn v1_tampered_chunk_fails_reassembly() {
        let (m, mut chunks) = seal_file_v1(b"important data");
        let last = chunks[0].len() - 1;
        chunks[0][last] ^= 0xFF;
        assert!(reassemble_and_verify(&m, &chunks).is_err());
    }

    #[test]
    fn v1_wrong_checksum_is_rejected() {
        let (mut m, chunks) = seal_file_v1(b"important data");
        m.checksum[0] ^= 0xFF;
        assert!(matches!(
            reassemble_and_verify(&m, &chunks),
            Err(FileError::ChecksumMismatch)
        ));
    }

    #[test]
    fn v1_reordered_chunks_are_rejected() {
        let mut data = vec![0u8; CHUNK_SIZE];
        data.resize(2 * CHUNK_SIZE, 1u8);
        data.resize(2 * CHUNK_SIZE + 5, 2u8);
        let (m, chunks) = seal_file_v1(&data);
        let reordered = vec![chunks[1].clone(), chunks[0].clone(), chunks[2].clone()];
        assert!(matches!(
            reassemble_and_verify(&m, &reordered),
            Err(FileError::ChecksumMismatch)
        ));
    }

    #[test]
    fn v1_missing_chunk_is_rejected() {
        let mut data = vec![0u8; CHUNK_SIZE];
        data.resize(2 * CHUNK_SIZE, 1u8);
        data.resize(2 * CHUNK_SIZE + 5, 2u8);
        let (m, chunks) = seal_file_v1(&data);
        let short = vec![chunks[0].clone(), chunks[1].clone()];
        assert!(matches!(
            reassemble_and_verify(&m, &short),
            Err(FileError::ChecksumMismatch)
        ));
    }

    #[test]
    fn v1_wrong_file_key_fails_to_open() {
        let (mut m, chunks) = seal_file_v1(b"important data");
        m.file_key[0] ^= 0xFF;
        assert!(matches!(
            reassemble_and_verify(&m, &chunks),
            Err(FileError::Decrypt)
        ));
    }
}
