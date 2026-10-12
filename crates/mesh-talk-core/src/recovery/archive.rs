//! Streaming encrypted archive envelope. Callers must write decrypted bytes only to
//! private staging and publish them only after `read_archive` returns successfully.

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"MTBACK1\0";
const VERSION: u8 = 1;
const HEADER_SIZE: usize = 8 + 1 + 16;
const FRAME_SIZE: usize = 1 + 8 + 4;
const TAG_SIZE: usize = 16;
const CHUNK_SIZE: usize = 64 * 1024;
const MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_CHUNKS: u64 = MAX_BYTES / CHUNK_SIZE as u64 + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveError {
    InvalidArchive,
    UnsupportedVersion,
    SizeLimit,
    Io,
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidArchive => f.write_str("invalid archive or recovery key"),
            Self::UnsupportedVersion => f.write_str("unsupported archive version"),
            Self::SizeLimit => f.write_str("archive exceeds size limit"),
            Self::Io => f.write_str("archive I/O error"),
        }
    }
}

impl std::error::Error for ArchiveError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveInfo {
    pub plaintext_bytes: u64,
    pub chunks: u64,
}

fn derive_cipher(
    key: &[u8; 32],
    salt: &[u8; 16],
    purpose: &[u8],
) -> Result<Aes256Gcm, ArchiveError> {
    let hkdf = Hkdf::<Sha256>::new(Some(salt), key);
    let mut derived = Zeroizing::new([0u8; 32]);
    hkdf.expand(purpose, &mut *derived)
        .map_err(|_| ArchiveError::InvalidArchive)?;
    Aes256Gcm::new_from_slice(&*derived).map_err(|_| ArchiveError::InvalidArchive)
}

fn nonce(salt: &[u8; 16], index: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[..4].copy_from_slice(&salt[..4]);
    nonce[4..].copy_from_slice(&index.to_be_bytes());
    nonce
}

fn frame_header(kind: u8, index: u64, length: u32) -> [u8; FRAME_SIZE] {
    let mut frame = [0u8; FRAME_SIZE];
    frame[0] = kind;
    frame[1..9].copy_from_slice(&index.to_be_bytes());
    frame[9..13].copy_from_slice(&length.to_be_bytes());
    frame
}

fn aad(header: &[u8; HEADER_SIZE], frame: &[u8; FRAME_SIZE]) -> [u8; HEADER_SIZE + FRAME_SIZE] {
    let mut aad = [0u8; HEADER_SIZE + FRAME_SIZE];
    aad[..HEADER_SIZE].copy_from_slice(header);
    aad[HEADER_SIZE..].copy_from_slice(frame);
    aad
}

fn seal(
    cipher: &Aes256Gcm,
    salt: &[u8; 16],
    header: &[u8; HEADER_SIZE],
    kind: u8,
    index: u64,
    plaintext: &[u8],
) -> Result<([u8; FRAME_SIZE], Vec<u8>), ArchiveError> {
    let len = u32::try_from(plaintext.len() + TAG_SIZE).map_err(|_| ArchiveError::SizeLimit)?;
    let frame = frame_header(kind, index, len);
    let nonce_bytes = nonce(salt, index);
    let nonce =
        Nonce::try_from(nonce_bytes.as_slice()).map_err(|_| ArchiveError::InvalidArchive)?;
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: &aad(header, &frame),
            },
        )
        .map_err(|_| ArchiveError::InvalidArchive)?;
    Ok((frame, ciphertext))
}

/// An encrypted sink for the logical snapshot writer. Dropping it without
/// `finish` deliberately leaves an archive without a valid final seal.
pub struct ArchiveWriter<W: Write> {
    destination: W,
    header: [u8; HEADER_SIZE],
    salt: [u8; 16],
    cipher: Aes256Gcm,
    final_cipher: Aes256Gcm,
    hasher: Sha256,
    buffer: Vec<u8>,
    info: ArchiveInfo,
}

impl<W: Write> ArchiveWriter<W> {
    pub fn new(mut destination: W, key: &[u8; 32]) -> Result<Self, ArchiveError> {
        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        let mut header = [0u8; HEADER_SIZE];
        header[..8].copy_from_slice(MAGIC);
        header[8] = VERSION;
        header[9..].copy_from_slice(&salt);
        destination
            .write_all(&header)
            .map_err(|_| ArchiveError::Io)?;
        Ok(Self {
            destination,
            header,
            salt,
            cipher: derive_cipher(key, &salt, b"mesh-talk-backup-v1/encryption")?,
            final_cipher: derive_cipher(key, &salt, b"mesh-talk-backup-v1/final")?,
            hasher: Sha256::new(),
            buffer: Vec::with_capacity(CHUNK_SIZE),
            info: ArchiveInfo {
                plaintext_bytes: 0,
                chunks: 0,
            },
        })
    }

    fn flush_chunk(&mut self) -> Result<(), ArchiveError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        if self.info.chunks >= MAX_CHUNKS {
            return Err(ArchiveError::SizeLimit);
        }
        let (frame, ciphertext) = seal(
            &self.cipher,
            &self.salt,
            &self.header,
            1,
            self.info.chunks,
            &self.buffer,
        )?;
        self.destination
            .write_all(&frame)
            .map_err(|_| ArchiveError::Io)?;
        self.destination
            .write_all(&ciphertext)
            .map_err(|_| ArchiveError::Io)?;
        self.info.chunks += 1;
        self.buffer.clear();
        Ok(())
    }

    pub fn push(&mut self, mut bytes: &[u8]) -> Result<(), ArchiveError> {
        self.info.plaintext_bytes = self
            .info
            .plaintext_bytes
            .checked_add(bytes.len() as u64)
            .ok_or(ArchiveError::SizeLimit)?;
        if self.info.plaintext_bytes > MAX_BYTES {
            return Err(ArchiveError::SizeLimit);
        }
        self.hasher.update(bytes);
        while !bytes.is_empty() {
            let take = bytes.len().min(CHUNK_SIZE - self.buffer.len());
            self.buffer.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.buffer.len() == CHUNK_SIZE {
                self.flush_chunk()?;
            }
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<ArchiveInfo, ArchiveError> {
        self.flush_chunk()?;
        let mut final_record = [0u8; 40];
        final_record[..8].copy_from_slice(&self.info.plaintext_bytes.to_be_bytes());
        final_record[8..].copy_from_slice(&self.hasher.finalize());
        let (frame, ciphertext) = seal(
            &self.final_cipher,
            &self.salt,
            &self.header,
            2,
            self.info.chunks,
            &final_record,
        )?;
        self.destination
            .write_all(&frame)
            .map_err(|_| ArchiveError::Io)?;
        self.destination
            .write_all(&ciphertext)
            .map_err(|_| ArchiveError::Io)?;
        self.destination.flush().map_err(|_| ArchiveError::Io)?;
        Ok(self.info)
    }
}

impl<W: Write> Write for ArchiveWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.push(bytes).map_err(std::io::Error::other)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.destination.flush()
    }
}

/// Encrypt a byte stream with a fresh archive salt. The 32-byte recovery key is
/// generated by the caller and must be saved separately from the archive.
pub fn write_archive<R: Read, W: Write>(
    mut source: R,
    destination: W,
    key: &[u8; 32],
) -> Result<ArchiveInfo, ArchiveError> {
    let mut writer = ArchiveWriter::new(destination, key)?;
    let mut buffer = [0u8; CHUNK_SIZE];
    loop {
        let size = source.read(&mut buffer).map_err(|_| ArchiveError::Io)?;
        if size == 0 {
            break;
        }
        writer.push(&buffer[..size])?;
    }
    writer.finish()
}

/// Decrypt into a **private staging sink**. Bytes in that sink are untrusted until
/// this function returns `Ok`; callers must discard staging on every error.
pub fn read_archive<R: Read, W: Write>(
    mut source: R,
    mut staging_sink: W,
    key: &[u8; 32],
) -> Result<ArchiveInfo, ArchiveError> {
    let mut header = [0u8; HEADER_SIZE];
    source
        .read_exact(&mut header)
        .map_err(|_| ArchiveError::InvalidArchive)?;
    if &header[..8] != MAGIC {
        return Err(ArchiveError::InvalidArchive);
    }
    if header[8] != VERSION {
        return Err(ArchiveError::UnsupportedVersion);
    }
    let salt: [u8; 16] = header[9..]
        .try_into()
        .map_err(|_| ArchiveError::InvalidArchive)?;
    let cipher = derive_cipher(key, &salt, b"mesh-talk-backup-v1/encryption")?;
    let final_cipher = derive_cipher(key, &salt, b"mesh-talk-backup-v1/final")?;
    let mut hasher = Sha256::new();
    let mut info = ArchiveInfo {
        plaintext_bytes: 0,
        chunks: 0,
    };
    loop {
        let mut frame = [0u8; FRAME_SIZE];
        source
            .read_exact(&mut frame)
            .map_err(|_| ArchiveError::InvalidArchive)?;
        let kind = frame[0];
        let index = u64::from_be_bytes(frame[1..9].try_into().unwrap());
        let length = u32::from_be_bytes(frame[9..13].try_into().unwrap()) as usize;
        if index != info.chunks || !matches!(kind, 1 | 2) {
            return Err(ArchiveError::InvalidArchive);
        }
        let max_length = if kind == 1 {
            CHUNK_SIZE + TAG_SIZE
        } else {
            40 + TAG_SIZE
        };
        if !(TAG_SIZE..=max_length).contains(&length) {
            return Err(ArchiveError::InvalidArchive);
        }
        let mut ciphertext = vec![0u8; length];
        source
            .read_exact(&mut ciphertext)
            .map_err(|_| ArchiveError::InvalidArchive)?;
        let nonce_bytes = nonce(&salt, index);
        let nonce =
            Nonce::try_from(nonce_bytes.as_slice()).map_err(|_| ArchiveError::InvalidArchive)?;
        let selected_cipher = if kind == 1 { &cipher } else { &final_cipher };
        let plaintext = selected_cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &ciphertext,
                    aad: &aad(&header, &frame),
                },
            )
            .map_err(|_| ArchiveError::InvalidArchive)?;
        if kind == 2 {
            if plaintext.len() != 40
                || plaintext[..8] != info.plaintext_bytes.to_be_bytes()
                || plaintext[8..] != hasher.finalize().as_slice()[..]
            {
                return Err(ArchiveError::InvalidArchive);
            }
            let mut trailing = [0u8; 1];
            if source.read(&mut trailing).map_err(|_| ArchiveError::Io)? != 0 {
                return Err(ArchiveError::InvalidArchive);
            }
            return Ok(info);
        }
        info.plaintext_bytes = info
            .plaintext_bytes
            .checked_add(plaintext.len() as u64)
            .ok_or(ArchiveError::SizeLimit)?;
        if info.plaintext_bytes > MAX_BYTES || info.chunks >= MAX_CHUNKS {
            return Err(ArchiveError::SizeLimit);
        }
        hasher.update(&plaintext);
        staging_sink
            .write_all(&plaintext)
            .map_err(|_| ArchiveError::Io)?;
        info.chunks += 1;
    }
}
