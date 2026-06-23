//! File sharing (files-over-event-log): a per-file key + chunked AES-256-GCM
//! (`crypto`) and the `FileManifest` data model (`manifest`). Pure — the node layer
//! carries chunks as events and seals the manifest with the conversation crypto.

pub mod crypto;
pub mod manifest;

pub use crypto::{
    chunk_count_for, chunk_hash, file_checksum, generate_file_nonce, open_chunk,
    open_chunk_indexed, seal_chunk, seal_chunk_indexed, split_chunks, FileError, FileKey,
    CHUNK_SIZE,
};
pub use manifest::{
    decode_manifest, open_chunk_for, reassemble_and_verify, AnyManifest, FileKind, FileManifest,
    FileManifestV2, FileManifestV3,
};
