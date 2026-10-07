//! A generic, password-encrypted, append-only record log shared by every on-disk
//! sidecar store (the event log, the sent/received plaintext logs, the ratchet
//! session store, the channel-sender store). It owns the *framing* and nothing
//! else: each store layers its own record type, magic, and in-memory index on top.
//!
//! ## On-disk format
//!
//! Header: a 6-byte `magic` + a 16-byte random `salt`. Body: a sequence of
//! length-prefixed AES-256-GCM records, one per stored value:
//! `[u32 be record_len][nonce(12)][ciphertext]`, where `ciphertext` is the
//! AES-GCM encryption of `bincode(R)`. Reuses [`crate::storage::encryption`].
//!
//! ## Trust boundary
//!
//! Each record is independently AEAD-authenticated, so a value's *content* cannot
//! be forged or altered on disk without the tampered record failing to decrypt
//! (→ [`LogError::CorruptFile`]). What this layer does NOT provide is
//! truncation/reorder resistance: the length prefixes are not authenticated, so a
//! corrupted mid-file frame causes the remainder to be treated as a torn tail and
//! dropped. A torn trailing record (crash mid-write) is likewise dropped.

use crate::eventlog::LogError;
use crate::storage::encryption::{
    decrypt_data, encrypt_data, generate_salt, EncryptionKey, NONCE_SIZE, SALT_SIZE,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

/// An append-only, password-encrypted record log generic over a serializable
/// record type `R`. Owns the live file handle, the derived key, the header salt,
/// the magic, and the path (retained so the log can be rewritten in place).
pub struct EncryptedRecordLog<R> {
    file: File,
    // Windows append handles lack FILE_WRITE_DATA, which set_len requires.
    // Retain a separate handle to the same file for repair, never reopen by path.
    #[cfg(windows)]
    repair: File,
    #[cfg(test)]
    append_fault: Option<File>,
    key: EncryptionKey,
    salt: [u8; SALT_SIZE],
    magic: [u8; 6],
    path: PathBuf,
    poisoned: bool,
    #[cfg(test)]
    pub(crate) before_rewrite_rename: Option<Box<dyn FnOnce() + Send + Sync>>,
    _marker: PhantomData<R>,
}

impl<R: Serialize + DeserializeOwned> EncryptedRecordLog<R> {
    #[cfg(test)]
    pub(crate) fn fail_appends_for_test(&mut self, enabled: bool) -> std::io::Result<()> {
        self.append_fault = if enabled {
            Some(File::open(&self.path)?)
        } else {
            None
        };
        Ok(())
    }
    /// Open the log at `path`, creating it (with a fresh random salt and `magic`
    /// header) if absent, else verifying the magic and loading + decrypting every
    /// stored record (tolerating a torn trailing record). Returns the writer plus
    /// the records already stored, in file order.
    pub fn open(path: &Path, password: &str, magic: &[u8; 6]) -> Result<(Self, Vec<R>), LogError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Atomically create-or-detect: `create_new` fails if the file exists,
        // so there is no exists()-then-create race.
        match OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut file) => {
                #[cfg(windows)]
                let repair = OpenOptions::new().write(true).open(path)?;
                let salt = generate_salt();
                let key = EncryptionKey::from_password(password, &salt)?;
                file.write_all(magic)?;
                file.write_all(&salt)?;
                file.sync_all()?;
                sync_parent(path)?;
                Ok((
                    Self {
                        file,
                        #[cfg(windows)]
                        repair,
                        #[cfg(test)]
                        append_fault: None,
                        key,
                        salt,
                        magic: *magic,
                        path: path.to_path_buf(),
                        poisoned: false,
                        #[cfg(test)]
                        before_rewrite_rename: None,
                        _marker: PhantomData,
                    },
                    Vec::new(),
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Self::load(path, password, magic)
            }
            Err(e) => Err(e.into()),
        }
    }

    fn load(path: &Path, password: &str, magic: &[u8; 6]) -> Result<(Self, Vec<R>), LogError> {
        let mut file = OpenOptions::new().read(true).append(true).open(path)?;
        #[cfg(windows)]
        let repair = OpenOptions::new().write(true).open(path)?;
        let mut got_magic = [0u8; 6];
        file.read_exact(&mut got_magic)
            .map_err(|_| LogError::CorruptFile("missing magic".into()))?;
        if &got_magic != magic {
            return Err(LogError::CorruptFile("bad magic".into()));
        }
        let mut salt = [0u8; SALT_SIZE];
        file.read_exact(&mut salt)
            .map_err(|_| LogError::CorruptFile("missing salt".into()))?;
        let key = EncryptionKey::from_password(password, &salt)?;

        let mut rest = Vec::new();
        file.read_to_end(&mut rest)?;
        let (records, valid) = Self::parse_records(&rest, &key)?;
        if valid != rest.len() {
            #[cfg(windows)]
            let repair_file = &repair;
            #[cfg(not(windows))]
            let repair_file = &file;
            repair_file.set_len((6 + SALT_SIZE + valid) as u64)?;
            repair_file.sync_all()?;
        }
        Ok((
            Self {
                file,
                #[cfg(windows)]
                repair,
                #[cfg(test)]
                append_fault: None,
                key,
                salt,
                magic: *magic,
                path: path.to_path_buf(),
                poisoned: false,
                #[cfg(test)]
                before_rewrite_rename: None,
                _marker: PhantomData,
            },
            records,
        ))
    }

    /// Parse the record region. A torn trailing record (crash mid-write) is
    /// dropped; a record that fails to decrypt (tampering) is an error.
    fn parse_records(mut data: &[u8], key: &EncryptionKey) -> Result<(Vec<R>, usize), LogError> {
        let mut records = Vec::new();
        let mut valid = 0;
        loop {
            if data.len() < 4 {
                break; // clean end, or a torn length prefix
            }
            let len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
            data = &data[4..];
            if data.len() < len {
                break; // torn trailing record — drop it
            }
            let record = &data[..len];
            data = &data[len..];

            if record.len() < NONCE_SIZE {
                return Err(LogError::CorruptFile("record shorter than nonce".into()));
            }
            let nonce: [u8; NONCE_SIZE] = record[..NONCE_SIZE]
                .try_into()
                .expect("nonce length checked");
            let ciphertext = &record[NONCE_SIZE..];
            let plaintext = decrypt_data(ciphertext, &nonce, key)
                .map_err(|_| LogError::CorruptFile("record failed to decrypt".into()))?;
            let value: R = bincode::deserialize(&plaintext)
                .map_err(|e| LogError::CorruptFile(format!("record decode: {e}")))?;
            records.push(value);
            valid += 4 + len;
        }
        Ok((records, valid))
    }

    /// Encrypt one record into its `[u32 len][nonce][ciphertext]` wire form.
    fn encode_record(&self, record: &R) -> Result<Vec<u8>, LogError> {
        let plaintext =
            bincode::serialize(record).map_err(|e| LogError::Serialization(e.to_string()))?;
        let (ciphertext, nonce) = encrypt_data(&plaintext, &self.key)?;
        let record_len = u32::try_from(NONCE_SIZE + ciphertext.len())
            .map_err(|_| LogError::Serialization("record exceeds u32 length".into()))?;
        let mut buf = Vec::with_capacity(4 + NONCE_SIZE + ciphertext.len());
        buf.extend_from_slice(&record_len.to_be_bytes());
        buf.extend_from_slice(&nonce);
        buf.extend_from_slice(&ciphertext);
        Ok(buf)
    }

    /// Append one record as an encrypted, length-prefixed frame, flushing it.
    pub fn append(&mut self, record: &R) -> Result<(), LogError> {
        self.append_inner(record, false)
    }

    /// Append and sync the complete encrypted frame before acknowledging success.
    /// Unix also syncs the containing directory; other platforms do not provide
    /// a directory durability guarantee here. Ancestor directories must already
    /// be durable (for example, the host's existing profile directory).
    pub fn append_durable(&mut self, record: &R) -> Result<(), LogError> {
        self.append_inner(record, true)
    }

    /// Sync all successful appends. A poisoned handle must first be reopened.
    pub fn sync(&self) -> Result<(), LogError> {
        if self.poisoned {
            return Err(LogError::CorruptFile("record log requires reopen".into()));
        }
        self.file.sync_all()?;
        sync_parent(&self.path)?;
        Ok(())
    }

    fn append_inner(&mut self, record: &R, durable: bool) -> Result<(), LogError> {
        self.append_with(record, durable, |file, buf| file.write_all(buf))
    }

    fn append_with(
        &mut self,
        record: &R,
        durable: bool,
        write: impl FnOnce(&mut File, &[u8]) -> std::io::Result<()>,
    ) -> Result<(), LogError> {
        if self.poisoned {
            return Err(LogError::CorruptFile("record log requires reopen".into()));
        }
        let buf = self.encode_record(record)?;
        let boundary = self.file.metadata()?.len();
        #[cfg(test)]
        let write_result = match self.append_fault.as_mut() {
            Some(readonly) => readonly.write_all(&buf),
            None => write(&mut self.file, &buf),
        };
        #[cfg(not(test))]
        let write_result = write(&mut self.file, &buf);
        let result = write_result.and_then(|()| {
            if durable {
                self.file.sync_all().and_then(|()| sync_parent(&self.path))
            } else {
                self.file.flush()
            }
        });
        if let Err(error) = result {
            #[cfg(windows)]
            let repair_file = &self.repair;
            #[cfg(not(windows))]
            let repair_file = &self.file;
            if repair_file
                .set_len(boundary)
                .and_then(|()| repair_file.sync_all())
                .is_err()
            {
                self.poisoned = true;
            }
            return Err(error.into());
        }
        Ok(())
    }

    /// Compact the file in place to exactly `records` (same header salt/key/magic,
    /// fresh per-record nonces). Written to a temp file and atomically renamed over
    /// the original, so an interrupted rewrite leaves the old file intact.
    pub fn rewrite(&mut self, records: &[R]) -> Result<(), LogError> {
        let tmp = self.path.with_extension("compact-tmp");
        {
            let f = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)?;
            let mut writer = BufWriter::new(f);
            writer.write_all(&self.magic)?;
            writer.write_all(&self.salt)?;
            for record in records {
                writer.write_all(&self.encode_record(record)?)?;
            }
            let f = writer
                .into_inner()
                .map_err(|error| LogError::Io(error.into_error()))?;
            f.sync_all()?;
        }
        // Reopen before replacing the live path. If this fails, the original log is
        // untouched and the caller can safely retry the compaction.
        let f = OpenOptions::new().read(true).append(true).open(&tmp)?;
        #[cfg(windows)]
        let repair = OpenOptions::new().write(true).open(&tmp)?;
        #[cfg(test)]
        if let Some(hook) = self.before_rewrite_rename.take() {
            hook();
        }
        std::fs::rename(&tmp, &self.path)?;
        // Keep the already-open handle: it refers to the rewritten inode even after the
        // rename, so there is no post-rename reopen failure that could leave `self.file`
        // pointing at the unlinked old inode.
        self.file = f;
        #[cfg(windows)]
        {
            self.repair = repair;
        }
        // Replacement has committed. Reporting failure here would leave callers'
        // indexes describing the old file. Directory sync is best effort after rename.
        if sync_parent(&self.path).is_err() {
            log::warn!("record log replacement directory sync failed");
        }
        Ok(())
    }
}

fn sync_parent(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        File::open(parent)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Rec {
        n: u64,
        data: Vec<u8>,
    }

    const TEST_MAGIC: &[u8; 6] = b"MTTEST";

    fn rec(n: u64, data: &[u8]) -> Rec {
        Rec {
            n,
            data: data.to_vec(),
        }
    }

    #[test]
    fn readonly_write_fault_retries_without_poisoning_live_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        let (mut log, _) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
        log.append_durable(&rec(1, b"before")).unwrap();
        log.fail_appends_for_test(true).unwrap();
        for _ in 0..2 {
            assert!(matches!(
                log.append_durable(&rec(2, b"failed")),
                Err(LogError::Io(_))
            ));
            assert!(!log.poisoned);
        }
        log.fail_appends_for_test(false).unwrap();
        log.append_durable(&rec(3, b"after")).unwrap();
        drop(log);
        let (_, records) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
        assert_eq!(records, vec![rec(1, b"before"), rec(3, b"after")]);
    }

    #[test]
    fn rewrite_replaces_repair_handle_and_preserves_native_append() {
        use std::io::{Seek, SeekFrom};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        let (mut log, _) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
        log.append_durable(&rec(1, b"old")).unwrap();
        log.rewrite(&[rec(2, b"replacement")]).unwrap();
        log.file.seek(SeekFrom::Start(0)).unwrap();
        log.append_durable(&rec(3, b"appended")).unwrap();
        assert!(log
            .append_with(&rec(4, b"partial"), true, |file, bytes| {
                file.write_all(&bytes[..7])?;
                Err(std::io::Error::other("partial write"))
            })
            .is_err());
        assert!(!log.poisoned);
        log.append_durable(&rec(5, b"after rollback")).unwrap();
        drop(log);
        let (_, records) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
        assert_eq!(
            records,
            vec![
                rec(2, b"replacement"),
                rec(3, b"appended"),
                rec(5, b"after rollback")
            ]
        );
    }

    #[test]
    fn partial_write_failure_rolls_back_before_next_durable_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        let (mut log, _) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
        log.append_durable(&rec(1, b"before")).unwrap();
        assert!(log
            .append_with(&rec(2, b"failed"), true, |file, buf| {
                file.write_all(&buf[..7])?;
                Err(std::io::Error::other("injected partial write"))
            })
            .is_err());
        log.append_durable(&rec(3, b"after")).unwrap();
        let (_, records) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
        assert_eq!(records, vec![rec(1, b"before"), rec(3, b"after")]);
    }

    #[test]
    fn failed_rollback_poison_fails_closed_until_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        let (mut log, _) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
        log.append_durable(&rec(1, b"before")).unwrap();
        log.file = File::open(&path).unwrap();
        #[cfg(windows)]
        {
            log.repair = File::open(&path).unwrap();
        }
        assert!(log.append(&rec(2, b"failed")).is_err());
        assert!(log.poisoned);
        assert!(log.append_durable(&rec(3, b"poisoned")).is_err());
        assert!(log.sync().is_err());
        drop(log);
        let (mut log, records) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
        assert_eq!(records, vec![rec(1, b"before")]);
        log.append_durable(&rec(4, b"after reopen")).unwrap();
        let (_, records) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
        assert_eq!(records, vec![rec(1, b"before"), rec(4, b"after reopen")]);
    }

    #[cfg(unix)]
    #[test]
    fn missing_parent_at_durable_boundary_rolls_back_and_allows_retry() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("original");
        let moved = dir.path().join("moved");
        let path = parent.join("t.log");
        let (mut log, _) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
        log.append_durable(&rec(1, b"before")).unwrap();
        std::fs::rename(&parent, &moved).unwrap();
        assert!(log.append_durable(&rec(2, b"failed")).is_err());
        assert!(log.sync().is_err());
        std::fs::rename(&moved, &parent).unwrap();
        log.append_durable(&rec(3, b"after")).unwrap();
        let (_, records) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
        assert_eq!(records, vec![rec(1, b"before"), rec(3, b"after")]);
    }

    #[test]
    fn header_layout_is_golden_bytes() {
        // The framing must produce a fixed header: the 6-byte magic followed by a
        // 16-byte salt. The salt is random, so we only pin the magic prefix and the
        // total header length — both of which are load-bearing for cross-version reads.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        let (_log, existing): (EncryptedRecordLog<Rec>, _) =
            EncryptedRecordLog::open(&path, "pw", TEST_MAGIC).unwrap();
        assert!(existing.is_empty());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 6 + SALT_SIZE, "header is magic(6) + salt(16)");
        assert_eq!(&bytes[..6], TEST_MAGIC);
    }

    #[test]
    fn appended_record_has_length_prefixed_nonce_framing() {
        // Pin the per-record framing: a u32-be length prefix whose value equals the
        // remaining record bytes, of which the first 12 are the nonce. The ciphertext
        // is random, so we assert the structural invariant, not the bytes.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        {
            let (mut log, _): (EncryptedRecordLog<Rec>, _) =
                EncryptedRecordLog::open(&path, "pw", TEST_MAGIC).unwrap();
            log.append(&rec(1, b"hi")).unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let body = &bytes[6 + SALT_SIZE..];
        let len = u32::from_be_bytes([body[0], body[1], body[2], body[3]]) as usize;
        assert_eq!(body.len(), 4 + len, "exactly one record, fully present");
        assert!(len > NONCE_SIZE, "record carries nonce + ciphertext + tag");
    }

    #[test]
    fn append_then_reopen_returns_records_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        {
            let (mut log, existing): (EncryptedRecordLog<Rec>, _) =
                EncryptedRecordLog::open(&path, "pw", TEST_MAGIC).unwrap();
            assert!(existing.is_empty());
            log.append(&rec(1, b"one")).unwrap();
            log.append(&rec(2, b"two")).unwrap();
        }
        let (_log, records): (EncryptedRecordLog<Rec>, _) =
            EncryptedRecordLog::open(&path, "pw", TEST_MAGIC).unwrap();
        assert_eq!(records, vec![rec(1, b"one"), rec(2, b"two")]);
    }

    #[test]
    fn wrong_password_fails_to_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        {
            let (mut log, _): (EncryptedRecordLog<Rec>, _) =
                EncryptedRecordLog::open(&path, "right", TEST_MAGIC).unwrap();
            log.append(&rec(1, b"secret")).unwrap();
        }
        let r: Result<(EncryptedRecordLog<Rec>, _), _> =
            EncryptedRecordLog::open(&path, "wrong", TEST_MAGIC);
        assert!(matches!(r, Err(LogError::CorruptFile(_))));
    }

    #[test]
    fn bad_magic_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        std::fs::write(&path, b"XXXXXXnot-a-log").unwrap();
        let r: Result<(EncryptedRecordLog<Rec>, _), _> =
            EncryptedRecordLog::open(&path, "pw", TEST_MAGIC);
        assert!(matches!(r, Err(LogError::CorruptFile(_))));
    }

    #[test]
    fn torn_trailing_record_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        {
            let (mut log, _): (EncryptedRecordLog<Rec>, _) =
                EncryptedRecordLog::open(&path, "pw", TEST_MAGIC).unwrap();
            log.append(&rec(1, b"good")).unwrap();
        }
        // A crash mid-write: a length prefix claiming more bytes than follow.
        {
            let mut file = OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(&100u32.to_be_bytes()).unwrap();
            file.write_all(&[0u8; 10]).unwrap();
        }
        let (mut log, records): (EncryptedRecordLog<Rec>, _) =
            EncryptedRecordLog::open(&path, "pw", TEST_MAGIC).unwrap();
        assert_eq!(records, vec![rec(1, b"good")]);
        log.append(&rec(2, b"after")).unwrap();
        drop(log);
        let (_, records) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
        assert_eq!(records, vec![rec(1, b"good"), rec(2, b"after")]);
    }

    #[test]
    fn torn_length_prefix_is_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        {
            let (mut log, _): (EncryptedRecordLog<Rec>, _) =
                EncryptedRecordLog::open(&path, "pw", TEST_MAGIC).unwrap();
            log.append(&rec(1, b"good")).unwrap();
        }
        {
            let mut file = OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(&[0xAB, 0xCD]).unwrap(); // only 2 of 4 prefix bytes
        }
        let (_log, records): (EncryptedRecordLog<Rec>, _) =
            EncryptedRecordLog::open(&path, "pw", TEST_MAGIC).unwrap();
        assert_eq!(records, vec![rec(1, b"good")]);
    }

    #[test]
    fn tampered_record_fails_to_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        {
            let (mut log, _): (EncryptedRecordLog<Rec>, _) =
                EncryptedRecordLog::open(&path, "pw", TEST_MAGIC).unwrap();
            log.append(&rec(1, b"data")).unwrap();
        }
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();
        let r: Result<(EncryptedRecordLog<Rec>, _), _> =
            EncryptedRecordLog::open(&path, "pw", TEST_MAGIC);
        assert!(matches!(r, Err(LogError::CorruptFile(_))));
    }

    #[test]
    fn rewrite_compacts_to_subset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        let (mut log, _): (EncryptedRecordLog<Rec>, _) =
            EncryptedRecordLog::open(&path, "pw", TEST_MAGIC).unwrap();
        log.append(&rec(1, b"a")).unwrap();
        log.append(&rec(2, b"b")).unwrap();
        log.append(&rec(3, b"c")).unwrap();
        log.rewrite(&[rec(1, b"a"), rec(3, b"c")]).unwrap();
        // Reopen and confirm only the kept subset survives, and appends still work.
        drop(log);
        let (mut log, records): (EncryptedRecordLog<Rec>, _) =
            EncryptedRecordLog::open(&path, "pw", TEST_MAGIC).unwrap();
        assert_eq!(records, vec![rec(1, b"a"), rec(3, b"c")]);
        log.append(&rec(4, b"d")).unwrap();
        drop(log);
        let (_log, records): (EncryptedRecordLog<Rec>, _) =
            EncryptedRecordLog::open(&path, "pw", TEST_MAGIC).unwrap();
        assert_eq!(records, vec![rec(1, b"a"), rec(3, b"c"), rec(4, b"d")]);
    }

    #[test]
    fn rewrite_to_empty_leaves_header_only_and_appends_resume() {
        // Compacting away every record (the "drop the whole conversation" case) must
        // leave a valid header-only file that reopens empty, and appends afterwards
        // must work against the freshly-reopened handle.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        let (mut log, _): (EncryptedRecordLog<Rec>, _) =
            EncryptedRecordLog::open(&path, "pw", TEST_MAGIC).unwrap();
        log.append(&rec(1, b"a")).unwrap();
        log.append(&rec(2, b"b")).unwrap();
        log.rewrite(&[]).unwrap();
        // On-disk: exactly the header, no record bytes.
        assert_eq!(std::fs::read(&path).unwrap().len(), 6 + SALT_SIZE);
        // The live handle still works post-rewrite.
        log.append(&rec(3, b"c")).unwrap();
        drop(log);
        let (_log, records): (EncryptedRecordLog<Rec>, _) =
            EncryptedRecordLog::open(&path, "pw", TEST_MAGIC).unwrap();
        assert_eq!(records, vec![rec(3, b"c")]);
    }

    #[test]
    fn round_trips_a_record_larger_than_a_single_buffer() {
        // A record whose ciphertext is far bigger than typical must frame, persist,
        // and reload intact (the u32 length prefix and read_to_end path handle it).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        let big = rec(1, &vec![0xA5u8; 512 * 1024]);
        {
            let (mut log, _): (EncryptedRecordLog<Rec>, _) =
                EncryptedRecordLog::open(&path, "pw", TEST_MAGIC).unwrap();
            log.append(&big).unwrap();
            log.append(&rec(2, b"small after big")).unwrap();
        }
        let (_log, records): (EncryptedRecordLog<Rec>, _) =
            EncryptedRecordLog::open(&path, "pw", TEST_MAGIC).unwrap();
        assert_eq!(records, vec![big, rec(2, b"small after big")]);
    }
}
