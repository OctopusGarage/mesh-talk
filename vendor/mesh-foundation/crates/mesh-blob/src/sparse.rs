use crate::{BlobLayout, BlobVerifier, FileError, chunk_hash};

/// Staging storage for one authenticated object version. The host must isolate
/// different manifests and keep staged data private until final verification.
/// A successful write must survive restart. Interrupted writes may be visible
/// to the next reader and are revalidated before use.
pub trait ChunkStore {
    type Error;

    /// Return the stored plaintext for this index, bounded by the expected
    /// length. Return None for missing or incomplete records.
    fn read(&mut self, index: u32, expected_len: u32) -> Result<Option<Vec<u8>>, Self::Error>;
    fn write(&mut self, index: u32, plaintext: &[u8]) -> Result<(), Self::Error>;
}

#[derive(Debug)]
pub enum SparseReceiveError<E> {
    Invalid(FileError),
    Store(E),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ChunkAcceptance {
    Stored,
    AlreadyStored,
}

/// Receive independent plaintext chunks in any order after transport
/// authentication and, for encrypted content, after decryption. The caller
/// authenticates the manifest and owns peer selection and publication.
pub struct SparseBlobReceiver<'a> {
    pub(crate) layout: BlobLayout,
    checksum: [u8; 32],
    pub(crate) hashes: &'a [[u8; 32]],
    present: Vec<bool>,
    stored_chunks: u32,
}

impl<'a> SparseBlobReceiver<'a> {
    pub fn new(
        layout: BlobLayout,
        checksum: [u8; 32],
        hashes: &'a [[u8; 32]],
        max_chunks: u32,
    ) -> Result<Self, FileError> {
        if layout.chunk_count() > max_chunks || hashes.len() != layout.chunk_count() as usize {
            return Err(FileError::Malformed("invalid chunk hash count".into()));
        }
        let mut present = Vec::new();
        present
            .try_reserve_exact(hashes.len())
            .map_err(|_| FileError::Malformed("unable to allocate chunk progress".into()))?;
        present.resize(hashes.len(), false);
        Ok(Self {
            layout,
            checksum,
            hashes,
            present,
            stored_chunks: 0,
        })
    }

    /// Number of chunks validated from staging or written successfully.
    pub fn stored_chunks(&self) -> u32 {
        self.stored_chunks
    }

    /// Enumerate missing indices. The scheduler may intersect these with
    /// each source's availability and exclude its outstanding requests.
    pub fn missing(&self) -> impl Iterator<Item = u32> + '_ {
        self.missing_from(0)
    }

    pub(crate) fn missing_from(&self, start: u32) -> impl Iterator<Item = u32> + '_ {
        self.present
            .iter()
            .enumerate()
            .skip(start as usize)
            .filter_map(|(index, present)| (!present).then_some(index as u32))
    }

    fn check(&self, index: u32, bytes: &[u8]) -> Result<(), FileError> {
        if bytes.len() != self.layout.chunk_len(index)? as usize {
            return Err(FileError::Malformed(
                "chunk length differs from layout".into(),
            ));
        }
        if chunk_hash(bytes) != self.hashes[index as usize] {
            return Err(FileError::ChunkHashMismatch(index));
        }
        Ok(())
    }

    /// Rebuild sparse progress by checking staged bytes against this manifest.
    /// A corrupt staged chunk remains missing and can be replaced.
    pub fn restore<S: ChunkStore>(
        &mut self,
        store: &mut S,
    ) -> Result<(), SparseReceiveError<S::Error>> {
        self.present.fill(false);
        self.stored_chunks = 0;
        for index in 0..self.layout.chunk_count() {
            let length = self
                .layout
                .chunk_len(index)
                .map_err(SparseReceiveError::Invalid)?;
            let bytes = store
                .read(index, length)
                .map_err(SparseReceiveError::Store)?;
            if bytes.is_some_and(|bytes| self.check(index, &bytes).is_ok()) {
                self.present[index as usize] = true;
                self.stored_chunks += 1;
            }
        }
        Ok(())
    }

    pub fn accept<S: ChunkStore>(
        &mut self,
        store: &mut S,
        index: u32,
        bytes: &[u8],
    ) -> Result<ChunkAcceptance, SparseReceiveError<S::Error>> {
        self.check(index, bytes)
            .map_err(SparseReceiveError::Invalid)?;
        if self.present[index as usize] {
            return Ok(ChunkAcceptance::AlreadyStored);
        }
        store
            .write(index, bytes)
            .map_err(SparseReceiveError::Store)?;
        self.present[index as usize] = true;
        self.stored_chunks += 1;
        Ok(ChunkAcceptance::Stored)
    }

    /// Verify the assembled object in index order with one chunk in memory.
    /// The host must keep the store stable until its publication completes.
    pub fn verify<S: ChunkStore>(&self, store: &mut S) -> Result<(), SparseReceiveError<S::Error>> {
        if self.missing().next().is_some() {
            return Err(SparseReceiveError::Invalid(FileError::Malformed(
                "blob incomplete".into(),
            )));
        }
        let mut verifier = BlobVerifier::for_layout(self.layout, self.checksum);
        for index in 0..self.layout.chunk_count() {
            let length = self
                .layout
                .chunk_len(index)
                .map_err(SparseReceiveError::Invalid)?;
            let bytes = store
                .read(index, length)
                .map_err(SparseReceiveError::Store)?
                .ok_or_else(|| {
                    SparseReceiveError::Invalid(FileError::Malformed("staged chunk missing".into()))
                })?;
            self.check(index, &bytes)
                .map_err(SparseReceiveError::Invalid)?;
            verifier
                .push(index, &bytes)
                .map_err(SparseReceiveError::Invalid)?;
        }
        verifier.finish().map_err(SparseReceiveError::Invalid)
    }
}

/// File-per-chunk staging. Use a private directory dedicated to one manifest,
/// with a single writer. Create the directory durably before opening it.
pub struct DirectoryChunkStore(std::path::PathBuf);

impl DirectoryChunkStore {
    pub fn open(directory: impl Into<std::path::PathBuf>) -> std::io::Result<Self> {
        let directory = directory.into();
        if !std::fs::metadata(&directory)?.is_dir() {
            return Err(std::io::ErrorKind::NotADirectory.into());
        }
        Ok(Self(directory))
    }
}

impl ChunkStore for DirectoryChunkStore {
    type Error = std::io::Error;

    fn read(&mut self, index: u32, expected_len: u32) -> std::io::Result<Option<Vec<u8>>> {
        use std::io::Read;
        let file = match std::fs::File::open(self.0.join(index.to_string())) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.take(u64::from(expected_len) + 1)
            .read_to_end(&mut bytes)?;
        Ok((bytes.len() == expected_len as usize).then_some(bytes))
    }

    fn write(&mut self, index: u32, plaintext: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.0)?;
        temporary.write_all(plaintext)?;
        temporary.as_file().sync_all()?;
        temporary.persist(self.0.join(index.to_string()))?;
        #[cfg(unix)]
        std::fs::File::open(&self.0)?.sync_all()?;
        Ok(())
    }
}
