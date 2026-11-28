//! Compatibility exports for the existing file encryption API.

pub use mesh_blob::{
    CHUNK_SIZE, FileError, FileKey, chunk_count_for, chunk_hash, file_checksum,
    generate_file_nonce, open_chunk, open_chunk_indexed, seal_chunk, seal_chunk_indexed,
    split_chunks,
};
