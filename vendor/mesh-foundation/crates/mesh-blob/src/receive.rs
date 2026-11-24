use crate::FileError;
use sha2::{Digest, Sha256};

/// Verify an ordered stream of authenticated plaintext chunks using constant
/// memory. Callers stage the bytes and publish them only after `finish` succeeds.
/// To recover after restart, replay the retained contiguous prefix in order.
pub struct BlobVerifier {
    count: u32,
    size: u64,
    checksum: [u8; 32],
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
            next: 0,
            received: 0,
            digest: Sha256::new(),
        })
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
