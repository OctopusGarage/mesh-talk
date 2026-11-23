//! Mesh Talk record-log adapter preserving key and error contracts.

use super::encryption::derive_record_key;
use super::errors::StorageError;
use crate::eventlog::LogError;
use mesh_storage::record_log::{EncryptedRecordLog as SdkRecordLog, RecordLogError};
use serde::{Serialize, de::DeserializeOwned};
use std::path::Path;

#[cfg(test)]
pub(crate) use mesh_storage::record_log::{frame_observations, sync_observations};

pub struct EncryptedRecordLog<R>(SdkRecordLog<R>);

impl From<RecordLogError> for LogError {
    fn from(error: RecordLogError) -> Self {
        match error {
            RecordLogError::Io(error) => Self::Io(error),
            RecordLogError::Crypto(error) => Self::Storage(StorageError::from(error)),
            RecordLogError::Serialization(message) => Self::Serialization(message),
            RecordLogError::CorruptFile(message) => Self::CorruptFile(message),
        }
    }
}

impl<R: Serialize + DeserializeOwned> EncryptedRecordLog<R> {
    pub fn open(path: &Path, password: &str, magic: &[u8; 6]) -> Result<(Self, Vec<R>), LogError> {
        let mut records = Vec::new();
        let log = Self::open_with_visitor(path, password, magic, |record| {
            records.push(record);
            Ok(())
        })?;
        Ok((log, records))
    }

    pub(crate) fn open_with_visitor(
        path: &Path,
        password: &str,
        magic: &[u8; 6],
        mut visit: impl FnMut(R) -> Result<(), LogError>,
    ) -> Result<Self, LogError> {
        let mut visit_error = None;
        let log = SdkRecordLog::open_with_key_and_visitor(
            path,
            magic,
            |salt| derive_record_key(password, salt),
            |record| {
                visit(record).map_err(|error| {
                    visit_error = Some(error);
                    // Stop the SDK replay before it can repair the file. Return the
                    // original application error outside the callback boundary.
                    RecordLogError::Io(std::io::Error::other("record visitor aborted"))
                })
            },
        );
        if let Some(error) = visit_error {
            return Err(error);
        }
        Ok(Self(log?))
    }

    pub fn append(&mut self, record: &R) -> Result<(), LogError> {
        Ok(self.0.append(record)?)
    }

    pub fn append_durable(&mut self, record: &R) -> Result<(), LogError> {
        Ok(self.0.append_durable(record)?)
    }

    pub(crate) fn recover_failed_append(&mut self) -> Result<(), LogError> {
        Ok(self.0.recover_failed_append()?)
    }

    pub fn sync(&self) -> Result<(), LogError> {
        Ok(self.0.sync()?)
    }

    pub fn rewrite(&mut self, records: &[R]) -> Result<(), LogError> {
        Ok(self.0.rewrite(records)?)
    }

    pub fn append_batch_atomic(&mut self, records: &[R]) -> Result<(), LogError> {
        Ok(self.0.append_batch_atomic(records)?)
    }

    #[cfg(test)]
    pub(crate) fn fail_appends_for_test(&mut self, enabled: bool) -> std::io::Result<()> {
        self.0.fail_appends_for_test(enabled)
    }

    #[cfg(test)]
    pub(crate) fn fail_next_append_rollback_for_test(&mut self, bytes: usize) {
        self.0.fail_next_append_rollback_for_test(bytes);
    }

    #[cfg(test)]
    pub(crate) fn set_before_rewrite_rename(&mut self, hook: Box<dyn FnOnce() + Send + Sync>) {
        self.0.before_rewrite_rename = Some(hook);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visitor_error_preserves_type_and_torn_tail() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("records");
        let magic = b"MTTEST";
        let (mut log, _) = EncryptedRecordLog::<u64>::open(&path, "pw", magic).unwrap();
        log.append_durable(&42).unwrap();
        drop(log);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&[1, 2])
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        let result = EncryptedRecordLog::<u64>::open_with_visitor(&path, "pw", magic, |_| {
            Err(LogError::StaleCursor)
        });
        assert!(matches!(result, Err(LogError::StaleCursor)));
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}
