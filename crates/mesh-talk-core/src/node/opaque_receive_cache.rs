//! Disposable encrypted transfer hints, never an admission or delivery journal.

use crate::eventlog::{ConversationId, EventId, LogError, persist::PersistentEventLog};
use crate::identity::device::PublicIdentity;
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 6] = b"MTORC1";
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 250_000;

#[derive(Serialize, Deserialize)]
struct Record {
    device: PublicIdentity,
    account: String,
    mailbox: ConversationId,
    outer: EventId,
    inner: EventId,
}

struct Writer {
    log: EncryptedRecordLog<Record>,
    device: PublicIdentity,
    account: String,
    bytes: u64,
}

#[derive(Default)]
pub(super) struct OpaqueReceiveCache {
    writer: Option<Writer>,
    entries: HashMap<(ConversationId, EventId), EventId>,
}

impl OpaqueReceiveCache {
    fn path(log_path: &Path) -> PathBuf {
        let mut path = log_path.as_os_str().to_os_string();
        path.push(".opaque-receive");
        path.into()
    }

    /// Opening a corrupt, foreign or unavailable optimization must not prevent
    /// login. Preserve that file and use ordinary mailbox receiving for this run.
    pub(super) fn open(
        log_path: &Path,
        password: &str,
        device: &PublicIdentity,
        account: &str,
    ) -> Self {
        match Self::try_open(log_path, password, device, account) {
            Ok(cache) => cache,
            Err(error) => {
                log::debug!("opaque receive cache unavailable: {error}");
                Self::default()
            }
        }
    }

    fn try_open(
        log_path: &Path,
        password: &str,
        device: &PublicIdentity,
        account: &str,
    ) -> Result<Self, LogError> {
        let path = Self::path(log_path);
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.len() > MAX_BYTES => {
                return Err(LogError::CorruptFile(
                    "opaque receive cache byte limit".into(),
                ));
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
        let (log, records) = EncryptedRecordLog::<Record>::open(&path, password, MAGIC)?;
        if records.len() > MAX_ENTRIES {
            return Err(LogError::CorruptFile(
                "opaque receive cache entry limit".into(),
            ));
        }
        let mut entries = HashMap::new();
        for record in records {
            if record.device != *device || record.account != account {
                return Err(LogError::CorruptFile(
                    "opaque receive cache owner mismatch".into(),
                ));
            }
            entries.insert((record.mailbox, record.outer), record.inner);
        }
        Ok(Self {
            writer: Some(Writer {
                log,
                device: device.clone(),
                account: account.to_owned(),
                bytes: std::fs::metadata(&path)?.len(),
            }),
            entries,
        })
    }

    /// A stale cache cannot hide history lost through crash-tail recovery,
    /// retention, restored backups or manual removal of the main event log.
    pub(super) fn restored(
        &self,
        log: &PersistentEventLog,
    ) -> HashMap<ConversationId, HashSet<EventId>> {
        let mut seen: HashMap<_, HashSet<_>> = HashMap::new();
        for ((mailbox, outer), inner) in &self.entries {
            if log.has(inner) {
                seen.entry(*mailbox).or_default().insert(*outer);
            }
        }
        seen
    }

    /// Caller supplies only opened wrappers with inner events retained in the
    /// persistent log. Invalid wrappers, ignored controls and deferred admission
    /// never enter this store. No full-file rewrites or per-event fsync: hints
    /// may be lost on a crash, and replay always checks the actual inner log.
    pub(super) fn remember(&mut self, mailbox: ConversationId, pairs: &[(EventId, EventId)]) {
        for &(outer, inner) in pairs {
            if self.entries.contains_key(&(mailbox, outer)) {
                continue;
            }
            let Some(writer) = &mut self.writer else {
                break;
            };
            let record = Record {
                device: writer.device.clone(),
                account: writer.account.clone(),
                mailbox,
                outer,
                inner,
            };
            let bytes = bincode::serialized_size(&record).expect("fixed cache record") + 32;
            if self.entries.len() >= MAX_ENTRIES || writer.bytes.saturating_add(bytes) > MAX_BYTES {
                self.writer = None;
                break;
            }
            if let Err(error) = writer.log.append(&record) {
                log::debug!("opaque receive cache write deferred: {error}");
                self.writer = None;
                break;
            }
            writer.bytes += bytes;
            self.entries.insert((mailbox, outer), inner);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::{Event, EventKind};
    use crate::identity::device::DeviceIdentity;

    #[test]
    fn restored_hints_require_retained_inner_and_exact_owner() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("events");
        let identity = DeviceIdentity::generate();
        let device = identity.public();
        let mailbox = ConversationId::new([1; 32]);
        let outer = EventId::new([2; 32]);
        let event = Event::new(
            &identity,
            mailbox,
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![7],
        );
        let mut log = PersistentEventLog::open(&path, "pw").unwrap();
        let mut cache = OpaqueReceiveCache::open(&path, "pw", &device, "account-a");
        cache.remember(mailbox, &[(outer, event.id)]);
        drop(cache);
        let cache = OpaqueReceiveCache::open(&path, "pw", &device, "account-a");
        assert!(cache.restored(&log).is_empty());
        log.append_durable(event).unwrap();
        assert!(cache.restored(&log)[&mailbox].contains(&outer));
        drop(cache);
        for (owner, account) in [
            (device.clone(), "account-b"),
            (DeviceIdentity::generate().public(), "account-a"),
        ] {
            let foreign = OpaqueReceiveCache::open(&path, "pw", &owner, account);
            assert!(foreign.restored(&log).is_empty());
            assert!(foreign.writer.is_none());
        }
        assert!(
            OpaqueReceiveCache::open(&path, "pw", &device, "account-a").restored(&log)[&mailbox]
                .contains(&outer)
        );
    }

    #[test]
    fn corrupt_or_oversized_cache_is_preserved_and_disabled() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("events");
        let device = DeviceIdentity::generate().public();
        let mut cache = OpaqueReceiveCache::open(&path, "pw", &device, "a");
        cache.remember(
            ConversationId::new([1; 32]),
            &[(EventId::new([2; 32]), EventId::new([3; 32]))],
        );
        drop(cache);
        let cache_path = OpaqueReceiveCache::path(&path);
        let mut bytes = std::fs::read(&cache_path).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(&cache_path, &bytes).unwrap();
        let cache = OpaqueReceiveCache::open(&path, "pw", &device, "a");
        assert!(cache.writer.is_none());
        assert!(cache.entries.is_empty());
        assert_eq!(std::fs::read(&cache_path).unwrap(), bytes);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&cache_path)
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        let cache = OpaqueReceiveCache::open(&path, "pw", &device, "a");
        assert!(cache.writer.is_none());
        assert_eq!(std::fs::metadata(&cache_path).unwrap().len(), MAX_BYTES + 1);
    }

    #[test]
    fn failed_append_never_installs_a_hint_or_rewrites_history() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("events");
        let device = DeviceIdentity::generate().public();
        let mut cache = OpaqueReceiveCache::open(&path, "pw", &device, "a");
        cache
            .writer
            .as_mut()
            .unwrap()
            .log
            .fail_appends_for_test(true)
            .unwrap();
        cache.remember(
            ConversationId::new([1; 32]),
            &[(EventId::new([2; 32]), EventId::new([3; 32]))],
        );
        assert!(cache.writer.is_none());
        assert!(cache.entries.is_empty());
        assert_eq!(
            std::fs::metadata(OpaqueReceiveCache::path(&path))
                .unwrap()
                .len(),
            22
        );
    }

    #[test]
    fn duplicate_hints_do_not_append_and_full_cache_stops_writing() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("events");
        let device = DeviceIdentity::generate().public();
        let mailbox = ConversationId::new([1; 32]);
        let pair = (EventId::new([2; 32]), EventId::new([3; 32]));
        let mut cache = OpaqueReceiveCache::open(&path, "pw", &device, "a");
        cache.remember(mailbox, &[pair]);
        let cache_path = OpaqueReceiveCache::path(&path);
        let bytes = std::fs::read(&cache_path).unwrap();
        cache.remember(mailbox, &[pair]);
        assert_eq!(std::fs::read(&cache_path).unwrap(), bytes);
        // Exercise the append byte budget without allocating a 64-MiB fixture.
        cache.writer.as_mut().unwrap().bytes = MAX_BYTES;
        cache.remember(mailbox, &[(EventId::new([4; 32]), EventId::new([5; 32]))]);
        assert!(cache.writer.is_none());
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(std::fs::read(&cache_path).unwrap(), bytes);
    }
}
