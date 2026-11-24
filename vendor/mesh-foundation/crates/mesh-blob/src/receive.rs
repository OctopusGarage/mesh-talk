use crate::FileError;
use sha2::{Digest, Sha256};

/// Validated fixed-chunk layout, with one zero-byte chunk for an empty blob.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlobLayout {
    size: u64,
    chunk_size: u32,
    count: u32,
}

impl BlobLayout {
    pub fn new(size: u64, chunk_size: u32, max_size: u64) -> Result<Self, FileError> {
        if chunk_size == 0 || size > max_size {
            return Err(FileError::Malformed(
                "invalid blob size or chunk size".into(),
            ));
        }
        let count = u32::try_from(size.div_ceil(u64::from(chunk_size)).max(1))
            .map_err(|_| FileError::Malformed("blob exceeds the chunk index range".into()))?;
        Ok(Self {
            size,
            chunk_size,
            count,
        })
    }

    pub fn chunk_count(self) -> u32 {
        self.count
    }

    pub fn chunk_len(self, index: u32) -> Result<u32, FileError> {
        if index >= self.count {
            return Err(FileError::Malformed("chunk index exceeds layout".into()));
        }
        let offset = u64::from(index) * u64::from(self.chunk_size);
        Ok((self.size - offset).min(u64::from(self.chunk_size)) as u32)
    }
}

/// Verify an ordered stream of authenticated plaintext chunks using constant
/// memory. Callers stage the bytes and publish them only after `finish` succeeds.
/// To recover after restart, replay the retained contiguous prefix in order.
pub struct BlobVerifier {
    count: u32,
    size: u64,
    checksum: [u8; 32],
    layout: Option<BlobLayout>,
    next: u32,
    received: u64,
    digest: Sha256,
}

impl BlobVerifier {
    pub fn new(count: u32, size: u64, checksum: [u8; 32]) -> Result<Self, FileError> {
        if count == 0 {
            return Err(FileError::Malformed("missing chunk count".into()));
        }
        Ok(Self {
            count,
            size,
            checksum,
            layout: None,
            next: 0,
            received: 0,
            digest: Sha256::new(),
        })
    }

    /// Verify exact per-chunk lengths as well as total size and checksum.
    pub fn for_layout(layout: BlobLayout, checksum: [u8; 32]) -> Self {
        Self {
            count: layout.count,
            size: layout.size,
            checksum,
            layout: Some(layout),
            next: 0,
            received: 0,
            digest: Sha256::new(),
        }
    }

    /// The next required chunk index, or the declared count after all chunks.
    pub fn next_index(&self) -> u32 {
        self.next
    }

    /// Record one already authenticated chunk. Invalid input leaves progress
    /// unchanged so the caller can retry the required index.
    pub fn push(&mut self, index: u32, plaintext: &[u8]) -> Result<(), FileError> {
        if index != self.next || index >= self.count {
            return Err(FileError::Malformed("unexpected chunk index".into()));
        }
        if let Some(layout) = self.layout
            && plaintext.len() as u64 != u64::from(layout.chunk_len(index)?)
        {
            return Err(FileError::Malformed(
                "chunk length differs from layout".into(),
            ));
        }
        let received = self
            .received
            .checked_add(plaintext.len() as u64)
            .filter(|received| *received <= self.size)
            .ok_or_else(|| FileError::Malformed("file size exceeds manifest".into()))?;
        self.digest.update(plaintext);
        self.received = received;
        self.next += 1;
        Ok(())
    }

    pub fn finish(&self) -> Result<(), FileError> {
        if self.next != self.count || self.received != self.size {
            return Err(FileError::Malformed(
                "file incomplete or size mismatch".into(),
            ));
        }
        let actual: [u8; 32] = self.digest.clone().finalize().into();
        if actual != self.checksum {
            return Err(FileError::ChecksumMismatch);
        }
        Ok(())
    }
}
