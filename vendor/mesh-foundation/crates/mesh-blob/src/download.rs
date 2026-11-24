use crate::{ChunkStore, SparseBlobReceiver, SparseReceiveError};
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::{future::Future, num::NonZeroUsize};

/// An authorized source for the exact object version. Sources must bound
/// reads by `length` and enforce deadlines and current authorization.
pub trait ChunkSource {
    type Error;

    fn has_chunk(&self, index: u32) -> bool;

    fn fetch(&self, index: u32, length: u32) -> impl Future<Output = Result<Vec<u8>, Self::Error>>;
}

#[derive(Debug)]
pub enum DownloadError<E> {
    /// All advertised sources failed, returned invalid bytes, or lack the block.
    Unavailable(u32),
    Receive(SparseReceiveError<E>),
}

/// Diagnostic classification without source addresses or error payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChunkRejection {
    FetchFailed,
    WrongLength,
    HashMismatch,
}

/// Notifications scoped to one download operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DownloadEvent {
    SourceRejected { index: u32, reason: ChunkRejection },
    ChunkStored { index: u32, bytes: u32 },
    Verified,
}

impl SparseBlobReceiver<'_> {
    /// Pull a bounded number of independent chunks concurrently. Rotate the
    /// starting source by index and retry other sources on failure. Durable
    /// progress survives an error or cancellation; retry with updated sources.
    pub async fn download<S: ChunkStore, P: ChunkSource>(
        &mut self,
        store: &mut S,
        sources: &[P],
        concurrency: NonZeroUsize,
    ) -> Result<(), DownloadError<S::Error>> {
        self.download_observed(store, sources, concurrency, |_| {})
            .await
    }

    /// Observe source rejection, durable writes and final verification.
    /// The callback runs inline and must be fast and nonblocking. Chunks can
    /// complete out of order. Dropping the future emits no terminal event.
    pub async fn download_observed<S: ChunkStore, P: ChunkSource>(
        &mut self,
        store: &mut S,
        sources: &[P],
        concurrency: NonZeroUsize,
        observe: impl Fn(DownloadEvent),
    ) -> Result<(), DownloadError<S::Error>> {
        let observe = &observe;
        let layout = self.layout;
        let hashes = self.hashes;
        let fetch = |index| async move {
            let length = layout
                .chunk_len(index)
                .expect("index from validated layout");
            let start = (index as usize).checked_rem(sources.len()).unwrap_or(0);
            for source in sources[start..].iter().chain(&sources[..start]) {
                if !source.has_chunk(index) {
                    continue;
                }
                let reason = match source.fetch(index, length).await {
                    Err(_) => ChunkRejection::FetchFailed,
                    Ok(bytes) if bytes.len() != length as usize => ChunkRejection::WrongLength,
                    Ok(bytes) if crate::chunk_hash(&bytes) != hashes[index as usize] => {
                        ChunkRejection::HashMismatch
                    }
                    Ok(bytes) => return Ok((index, bytes)),
                };
                observe(DownloadEvent::SourceRejected { index, reason });
            }
            Err(DownloadError::Unavailable(index))
        };

        let mut next = 0;
        let mut unavailable = None;
        let mut requests = FuturesUnordered::new();
        loop {
            while unavailable.is_none()
                && requests.len() < concurrency.get()
                && next < layout.chunk_count()
            {
                let Some(index) = self.missing_from(next).next() else {
                    next = layout.chunk_count();
                    break;
                };
                requests.push(fetch(index));
                next = index + 1;
            }
            // Keep healthy sources moving as slots free up. After a terminal
            // miss, drain only work already started so it can survive a retry.
            match requests.next().await {
                Some(Ok((index, bytes))) => {
                    if self
                        .accept(store, index, &bytes)
                        .map_err(DownloadError::Receive)?
                        == crate::ChunkAcceptance::Stored
                    {
                        observe(DownloadEvent::ChunkStored {
                            index,
                            bytes: bytes.len() as u32,
                        });
                    }
                }
                Some(Err(error)) => {
                    unavailable.get_or_insert(error);
                }
                None => break,
            }
        }
        if let Some(error) = unavailable {
            return Err(error);
        }
        self.verify(store).map_err(DownloadError::Receive)?;
        observe(DownloadEvent::Verified);
        Ok(())
    }
}

/// Decrypt indexed ciphertext at the receiver. The wrapped source only needs
/// the encrypted chunks, alongside the transport's independent authorization.
pub struct EncryptedChunkSource<'a, P> {
    source: P,
    key: &'a crate::FileKey,
    nonce: [u8; 8],
}

impl<'a, P> EncryptedChunkSource<'a, P> {
    pub fn new(source: P, key: &'a crate::FileKey, nonce: [u8; 8]) -> Self {
        Self { source, key, nonce }
    }
}

#[derive(Debug)]
pub enum EncryptedChunkError<E> {
    Source(E),
    Invalid(crate::FileError),
}

impl<P: ChunkSource> ChunkSource for EncryptedChunkSource<'_, P> {
    type Error = EncryptedChunkError<P::Error>;

    fn has_chunk(&self, index: u32) -> bool {
        self.source.has_chunk(index)
    }

    async fn fetch(&self, index: u32, length: u32) -> Result<Vec<u8>, Self::Error> {
        let wire_length = length.checked_add(crate::TAG_SIZE as u32).ok_or_else(|| {
            EncryptedChunkError::Invalid(crate::FileError::Malformed(
                "encrypted chunk too large".into(),
            ))
        })?;
        let ciphertext = self
            .source
            .fetch(index, wire_length)
            .await
            .map_err(EncryptedChunkError::Source)?;
        if ciphertext.len() != wire_length as usize {
            return Err(EncryptedChunkError::Invalid(crate::FileError::Malformed(
                "unexpected encrypted chunk length".into(),
            )));
        }
        crate::open_chunk_indexed(self.key, &self.nonce, index, &ciphertext)
            .map_err(EncryptedChunkError::Invalid)
    }
}
