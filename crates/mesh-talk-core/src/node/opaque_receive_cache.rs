//! Optional transfer hints. Persistent inner-event evidence is rechecked only
//! for the active mailbox; session-only progress never survives a restart.
use crate::eventlog::{ConversationId, EventId, LogError, persist::PersistentEventLog};
use crate::identity::device::PublicIdentity;
use crate::node::transcript_repository::{
    OPAQUE_RECEIVE_HINTS, publication::PublishedHistorySource,
};
use crate::storage::{profile::context::ProfileStorage, record_log::EncryptedRecordLog};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

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

const SCHEMA: &str = "
CREATE TABLE opaque_hint_owner(singleton INTEGER PRIMARY KEY CHECK(singleton=1),ed25519 BLOB NOT NULL CHECK(length(ed25519)=32),x25519 BLOB NOT NULL CHECK(length(x25519)=32),account TEXT NOT NULL,records INTEGER NOT NULL DEFAULT 0 CHECK(records>=0),bytes INTEGER NOT NULL DEFAULT 0 CHECK(bytes>=0));
CREATE TABLE opaque_retained_hints(mailbox BLOB NOT NULL CHECK(length(mailbox)=32),outer_id BLOB NOT NULL CHECK(length(outer_id)=32),inner_id BLOB NOT NULL CHECK(length(inner_id)=32),PRIMARY KEY(mailbox,outer_id)) WITHOUT ROWID;
CREATE TABLE opaque_session_hints(mailbox BLOB NOT NULL CHECK(length(mailbox)=32),outer_id BLOB NOT NULL CHECK(length(outer_id)=32),session BLOB NOT NULL CHECK(length(session)=16),inner_id BLOB CHECK(inner_id IS NULL OR length(inner_id)=32),PRIMARY KEY(mailbox,outer_id)) WITHOUT ROWID;
CREATE INDEX opaque_session_epochs ON opaque_session_hints(session,mailbox,outer_id);
CREATE TABLE opaque_session_usage(singleton INTEGER PRIMARY KEY CHECK(singleton=1),records INTEGER NOT NULL DEFAULT 0 CHECK(records>=0));
";

pub(super) struct OpaqueReceiveCache {
    profile: Option<Arc<ProfileStorage>>,
    source: Option<PublishedHistorySource>,
    session: [u8; 16],
    record_bytes: u64,
    session_cleaned: bool,
}

impl OpaqueReceiveCache {
    fn path(log_path: &Path) -> PathBuf {
        let mut path = log_path.as_os_str().to_os_string();
        path.push(".opaque-receive");
        path.into()
    }

    /// A corrupt/foreign optional legacy cache remains untouched and cannot
    /// prevent login. Session progress remains available independently.
    pub(super) fn open(
        profile: Arc<ProfileStorage>,
        log_path: &Path,
        password: &str,
        device: &PublicIdentity,
        account: &str,
    ) -> Self {
        let session = *uuid::Uuid::new_v4().as_bytes();
        let mut owner = Self {
            profile: None,
            source: None,
            session,
            record_bytes: 0,
            session_cleaned: false,
        };
        if let Err(error) = Self::initialize(&profile) {
            log::debug!("opaque receive hints unavailable: {error}");
            return owner;
        }
        owner.profile = Some(profile.clone());
        let zero = [0; 32];
        let record = Record {
            device: device.clone(),
            account: account.to_owned(),
            mailbox: ConversationId::new(zero),
            outer: EventId::new(zero),
            inner: EventId::new(zero),
        };
        owner.record_bytes = bincode::serialized_size(&record).expect("fixed cache record") + 32;
        match Self::publish(profile, log_path, password, device, account) {
            Ok(source) => owner.source = Some(source),
            Err(error) => log::debug!("opaque receive cache unavailable: {error}"),
        }
        owner
    }

    fn initialize(profile: &ProfileStorage) -> Result<(), LogError> {
        profile.database().transaction(|c| {
            let exists: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='opaque_hint_owner')",
                [],
                |r| r.get(0),
            )?;
            if !exists {
                c.execute_batch(SCHEMA)?;
                c.execute("INSERT INTO opaque_session_usage(singleton) VALUES(1)", [])?;
            }
            crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
        })
    }

    fn publish(
        profile: Arc<ProfileStorage>,
        log_path: &Path,
        password: &str,
        device: &PublicIdentity,
        account: &str,
    ) -> Result<PublishedHistorySource, LogError> {
        let source = PublishedHistorySource::open(
            profile,
            &OpaqueReceiveCache::path(log_path),
            password,
            OPAQUE_RECEIVE_HINTS,
            |c, path, password| {
                if std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > MAX_BYTES) {
                    return Err(invalid("opaque receive cache byte limit"));
                }
                c.execute("INSERT INTO opaque_hint_owner(singleton,ed25519,x25519,account) VALUES(1,?1,?2,?3)", params![device.ed25519_pub.as_slice(),device.x25519_pub.as_slice(),account])?;
                let mut records = 0;
                let log = EncryptedRecordLog::<Record>::open_with_visitor(
                    path,
                    password,
                    MAGIC,
                    |record| {
                        records += 1;
                        if records > MAX_ENTRIES {
                            return Err(invalid("opaque receive cache entry limit"));
                        }
                        if record.device != *device || record.account != account {
                            return Err(invalid("opaque receive cache owner mismatch"));
                        }
                        let bytes =
                            bincode::serialized_size(&record).expect("fixed cache record") + 32;
                        if !Self::put_on(c, record.mailbox, record.outer, record.inner, bytes)? {
                            return Err(invalid("opaque receive cache byte limit"));
                        }
                        Ok(())
                    },
                )?;
                log.sync()
            },
        )?;
        let matches = source.profile().database().read(|c| {
            Ok(c.query_row("SELECT ed25519=?1 AND x25519=?2 AND account=?3 FROM opaque_hint_owner WHERE singleton=1",params![device.ed25519_pub.as_slice(),device.x25519_pub.as_slice(),account],|r|r.get::<_,bool>(0))?)
        })?;
        if !matches {
            return Err(invalid("opaque receive cache owner mismatch"));
        }
        Ok(source)
    }

    fn put_on(
        c: &Connection,
        mailbox: ConversationId,
        outer: EventId,
        inner: EventId,
        bytes: u64,
    ) -> Result<bool, LogError> {
        let existing: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM opaque_retained_hints WHERE mailbox=?1 AND outer_id=?2)",
            params![mailbox.as_bytes().as_slice(), outer.as_bytes().as_slice()],
            |r| r.get(0),
        )?;
        if existing {
            return Ok(true);
        }
        let (count, charged): (i64, i64) = c.query_row(
            "SELECT records,bytes FROM opaque_hint_owner WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let bytes =
            i64::try_from(bytes).map_err(|_| invalid("opaque hint record exceeds SQL range"))?;
        if count >= MAX_ENTRIES as i64 || charged.saturating_add(bytes) > MAX_BYTES as i64 {
            return Ok(false);
        }
        c.execute(
            "INSERT INTO opaque_retained_hints VALUES(?1,?2,?3)",
            params![
                mailbox.as_bytes().as_slice(),
                outer.as_bytes().as_slice(),
                inner.as_bytes().as_slice()
            ],
        )?;
        c.execute(
            "UPDATE opaque_hint_owner SET records=records+1,bytes=bytes+?1 WHERE singleton=1",
            [bytes],
        )?;
        Ok(true)
    }

    /// Materialization is confined to one active protocol round and the
    /// existing mailbox ID budget. Ordinary open reads no hint rows.
    pub(super) fn seen(
        &self,
        mailbox: ConversationId,
        log: &PersistentEventLog,
    ) -> Result<HashSet<EventId>, LogError> {
        let Some(profile) = &self.profile else {
            return Ok(HashSet::new());
        };
        let mut seen = HashSet::new();
        let mut after: Option<EventId> = None;
        loop {
            let page = profile.database().read(|c| {
                let mut statement = c.prepare_cached("SELECT outer_id,inner_id FROM opaque_session_hints WHERE mailbox=?1 AND outer_id>?2 AND session=?3 ORDER BY outer_id LIMIT 128")?;
                let mut rows = statement.query(params![mailbox.as_bytes().as_slice(),after.as_ref().map(|id|id.as_bytes().as_slice()).unwrap_or(&[]),self.session.as_slice()])?;
                let mut result = Vec::with_capacity(128);
                while let Some(row) = rows.next()? { let inner = if matches!(row.get_ref(1)?, rusqlite::types::ValueRef::Null) { None } else { Some(id(row,1)?) };
                    result.push((id(row, 0)?, inner)); }
                Ok(result)
            })?;
            if page.is_empty() {
                break;
            }
            for (outer, inner) in page {
                after = Some(outer);
                if match inner {
                    Some(inner) => log.has(&inner)?,
                    None => true,
                } {
                    seen.insert(outer);
                }
            }
        }
        if self.source.is_none() {
            return Ok(seen);
        }
        after = None;
        while seen.len() < MAX_ENTRIES {
            let page = profile.database().read(|c| {
                let mut statement = c.prepare_cached("SELECT outer_id,inner_id FROM opaque_retained_hints WHERE mailbox=?1 AND outer_id>?2 ORDER BY outer_id LIMIT 128")?;
                let mut rows = statement.query(params![mailbox.as_bytes().as_slice(),after.as_ref().map(|id|id.as_bytes().as_slice()).unwrap_or(&[])])?;
                let mut result = Vec::with_capacity(128);
                while let Some(row) = rows.next()? { result.push((id(row,0)?,id(row,1)?)); }
                Ok(result)
            })?;
            if page.is_empty() {
                break;
            }
            for (outer, inner) in page {
                after = Some(outer);
                if log.has(&inner)? {
                    seen.insert(outer);
                }
                if seen.len() == MAX_ENTRIES {
                    break;
                }
            }
        }
        Ok(seen)
    }

    pub(super) fn remember(&mut self, mailbox: ConversationId, pairs: &[(EventId, EventId)]) {
        let persisted = if let Some(source) = &self.source {
            source.check_write().and_then(|()| {
                for page in pairs.chunks(128) {
                    let full = source.profile().database().transaction(|c| {
                        for &(outer, inner) in page {
                            if !Self::put_on(c, mailbox, outer, inner, self.record_bytes)? {
                                return Ok(true);
                            }
                        }
                        Ok(false)
                    })?;
                    if full {
                        return Ok(false);
                    }
                }
                Ok(true)
            })
        } else {
            Ok(false)
        };
        if let Err(error) = &persisted {
            log::debug!("opaque receive hint write deferred: {error}");
        }
        if !matches!(persisted, Ok(true)) {
            self.remember_progress(
                mailbox,
                pairs.iter().map(|(outer, inner)| (*outer, Some(*inner))),
            );
        }
    }

    pub(super) fn remember_session(&mut self, mailbox: ConversationId, ids: &[EventId]) {
        self.remember_progress(mailbox, ids.iter().map(|id| (*id, None)));
    }

    fn remember_progress(
        &mut self,
        mailbox: ConversationId,
        mut progress: impl Iterator<Item = (EventId, Option<EventId>)>,
    ) {
        let Some(profile) = &self.profile else {
            return;
        };
        loop {
            let page: Vec<_> = progress.by_ref().take(128).collect();
            if page.is_empty() {
                break;
            }
            let result = profile.database().transaction(|c| {
                // Retire a bounded batch of another run's optional hints.
                let removed = if self.session_cleaned { 0 } else {
                    c.execute("DELETE FROM opaque_session_hints WHERE (mailbox,outer_id) IN (SELECT mailbox,outer_id FROM opaque_session_hints WHERE session<?1 UNION ALL SELECT mailbox,outer_id FROM opaque_session_hints WHERE session>?1 LIMIT 128)", [self.session.as_slice()])?
                };
                c.execute("UPDATE opaque_session_usage SET records=records-?1 WHERE singleton=1", [removed as i64])?;
                let mut count: i64 = c.query_row("SELECT records FROM opaque_session_usage WHERE singleton=1", [], |r| r.get(0))?;
                for (outer, inner) in page {
                    let args = params![mailbox.as_bytes().as_slice(), outer.as_bytes().as_slice(), self.session.as_slice(), inner.as_ref().map(|id|id.as_bytes().as_slice())];
                    let updated = c.execute("UPDATE opaque_session_hints SET session=?3,inner_id=?4 WHERE mailbox=?1 AND outer_id=?2", args)?;
                    if updated != 0 || count >= MAX_ENTRIES as i64 { continue; }
                    c.execute("INSERT INTO opaque_session_hints VALUES(?1,?2,?3,?4)", args)?;
                    count += 1;
                }
                c.execute("UPDATE opaque_session_usage SET records=?1 WHERE singleton=1", [count])?;
                Ok(removed == 0)
            });
            match result {
                Ok(cleaned) => self.session_cleaned |= cleaned,
                Err(error) => {
                    log::debug!("opaque session hint write deferred: {error}");
                    break;
                }
            }
        }
    }
}

fn id(row: &rusqlite::Row<'_>, index: usize) -> Result<EventId, LogError> {
    let value = row
        .get_ref(index)?
        .as_blob()
        .map_err(|_| invalid("invalid opaque hint key"))?;
    Ok(EventId::new(
        value
            .try_into()
            .map_err(|_| invalid("invalid opaque hint key"))?,
    ))
}
fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::{Event, EventKind};
    use crate::identity::{account::Account, device::DeviceIdentity};
    use crate::storage::profile::ProfileStorageLease;

    fn profile(root: &Path, device: &DeviceIdentity, account: &Account) -> Arc<ProfileStorage> {
        let profile = ProfileStorage::open_participant(
            ProfileStorageLease::acquire(root).unwrap(),
            &device.public(),
            &account.public(),
            &root.join("events"),
            &root.join("sent"),
            "pw",
        )
        .unwrap();
        profile.complete_participant("pw", None).unwrap();
        profile
    }
    fn id(n: u64) -> EventId {
        let mut id = [0; 32];
        id[..8].copy_from_slice(&n.to_be_bytes());
        EventId::new(id)
    }
    fn event(identity: &DeviceIdentity) -> Event {
        Event::new(
            identity,
            ConversationId::new([1; 32]),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![7],
        )
    }
    fn legacy(path: &Path, device: &PublicIdentity, account: &str, count: u64, inner: EventId) {
        let (mut log, _) =
            EncryptedRecordLog::<Record>::open(&OpaqueReceiveCache::path(path), "pw", MAGIC)
                .unwrap();
        for n in 0..count {
            log.append(&Record {
                device: device.clone(),
                account: account.into(),
                mailbox: ConversationId::new([1; 32]),
                outer: id(n),
                inner,
            })
            .unwrap();
        }
        log.sync().unwrap();
    }

    #[test]
    fn warm_open_keeps_hints_on_disk() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("events");
        let device = DeviceIdentity::generate();
        let account = Account::generate();
        let event = event(&device);
        let mailbox = event.conversation_id;
        legacy(
            &path,
            &device.public(),
            &account.account_id(),
            512,
            event.id,
        );
        let owner = profile(root.path(), &device, &account);
        let mut log = PersistentEventLog::open_profile(owner.clone(), "pw").unwrap();
        log.append_durable(event).unwrap();
        let mut cache = OpaqueReceiveCache::open(
            owner.clone(),
            &path,
            "pw",
            &device.public(),
            &account.account_id(),
        );
        assert_eq!(cache.seen(mailbox, &log).unwrap().len(), 512);
        cache.remember_session(mailbox, &[id(600)]);
        assert!(cache.seen(mailbox, &log).unwrap().contains(&id(600)));
        drop(cache);
        drop(log);
        drop(owner);
        crate::storage::record_log::frame_observations::reset();
        let owner = profile(root.path(), &device, &account);
        let log = PersistentEventLog::open_profile(owner.clone(), "pw").unwrap();
        let cache =
            OpaqueReceiveCache::open(owner, &path, "pw", &device.public(), &account.account_id());
        assert_eq!(
            crate::storage::record_log::frame_observations::maximum_capacity(),
            0,
            "warm open must not decrypt historical transfer hints"
        );
        let seen = cache.seen(mailbox, &log).unwrap();
        assert_eq!(seen.len(), 512);
        assert!(
            !seen.contains(&id(600)),
            "transient wrapper progress must not survive restart"
        );
        assert_eq!(log.repository().unwrap().body_read_counts(), (0, 0));
    }

    #[test]
    fn hints_require_retained_inner_and_exact_owner() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("events");
        let device = DeviceIdentity::generate();
        let account = Account::generate();
        let owner = profile(root.path(), &device, &account);
        let mut log = PersistentEventLog::open_profile(owner.clone(), "pw").unwrap();
        let event = event(&device);
        let mailbox = event.conversation_id;
        let mut cache = OpaqueReceiveCache::open(
            owner.clone(),
            &path,
            "pw",
            &device.public(),
            &account.account_id(),
        );
        cache.remember(mailbox, &[(id(10), event.id)]);
        assert!(cache.seen(mailbox, &log).unwrap().is_empty());
        log.append_durable(event).unwrap();
        assert!(cache.seen(mailbox, &log).unwrap().contains(&id(10)));
        for (device, account) in [
            (device.public(), "foreign-account".to_string()),
            (DeviceIdentity::generate().public(), account.account_id()),
        ] {
            let mut foreign =
                OpaqueReceiveCache::open(owner.clone(), &path, "pw", &device, &account);
            assert!(foreign.source.is_none());
            assert!(foreign.seen(mailbox, &log).unwrap().is_empty());
            foreign.remember_session(mailbox, &[id(11)]);
            assert!(
                foreign.seen(mailbox, &log).unwrap().contains(&id(11)),
                "optional cache failure must retain session progress"
            );
        }
        assert!(cache.seen(mailbox, &log).unwrap().contains(&id(10)));
    }

    #[test]
    fn corrupt_or_foreign_legacy_cache_is_preserved_without_publication() {
        for corrupt in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("events");
            let device = DeviceIdentity::generate();
            let account = Account::generate();
            let legacy_account = if corrupt {
                account.account_id()
            } else {
                "foreign".to_owned()
            };
            legacy(&path, &device.public(), &legacy_account, 1, id(10));
            let cache_path = OpaqueReceiveCache::path(&path);
            let mut bytes = std::fs::read(&cache_path).unwrap();
            if corrupt {
                *bytes.last_mut().unwrap() ^= 1;
                std::fs::write(&cache_path, &bytes).unwrap();
            }
            let owner = profile(root.path(), &device, &account);
            let cache = OpaqueReceiveCache::open(
                owner.clone(),
                &path,
                "pw",
                &device.public(),
                &account.account_id(),
            );
            assert!(cache.source.is_none());
            assert_eq!(std::fs::read(&cache_path).unwrap(), bytes);
            let (rows, sources): (i64, i64) = owner
                .database()
                .read(|c| {
                    Ok((
                        c.query_row("SELECT count(*) FROM opaque_retained_hints", [], |r| {
                            r.get(0)
                        })?,
                        c.query_row(
                            "SELECT count(*) FROM transcript_sources WHERE kind=?1",
                            [OPAQUE_RECEIVE_HINTS],
                            |r| r.get(0),
                        )?,
                    ))
                })
                .unwrap();
            assert_eq!((rows, sources), (0, 0));
        }
    }

    #[test]
    fn unavailable_persistent_hints_keep_retryable_session_progress() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("events");
        let device = DeviceIdentity::generate();
        let account = Account::generate();
        let owner = profile(root.path(), &device, &account);
        let mut log = PersistentEventLog::open_profile(owner.clone(), "pw").unwrap();
        let event = event(&device);
        let mailbox = event.conversation_id;
        let inner = event.id;
        log.append_durable(event).unwrap();
        std::fs::write(OpaqueReceiveCache::path(&path), b"corrupt optional cache").unwrap();
        let mut cache =
            OpaqueReceiveCache::open(owner, &path, "pw", &device.public(), &account.account_id());
        assert!(cache.source.is_none());
        cache.remember(mailbox, &[(id(10), inner)]);
        assert!(
            cache.seen(mailbox, &log).unwrap().contains(&id(10)),
            "optional-cache failure cannot pin repeated payload rounds"
        );
        log.drop_conversation(&mailbox).unwrap();
        assert!(
            cache.seen(mailbox, &log).unwrap().is_empty(),
            "session hints must also depend on retained inner events"
        );
    }

    #[test]
    fn duplicate_or_failed_writes_never_invent_received_evidence() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("events");
        let device = DeviceIdentity::generate();
        let account = Account::generate();
        let owner = profile(root.path(), &device, &account);
        let mut log = PersistentEventLog::open_profile(owner.clone(), "pw").unwrap();
        let event = event(&device);
        let inner = event.id;
        let mailbox = event.conversation_id;
        log.append_durable(event).unwrap();
        let mut cache = OpaqueReceiveCache::open(
            owner.clone(),
            &path,
            "pw",
            &device.public(),
            &account.account_id(),
        );
        cache.remember(mailbox, &[(id(10), inner)]);
        cache.remember(mailbox, &[(id(10), inner)]);
        assert_eq!(cache.seen(mailbox, &log).unwrap().len(), 1);
        owner.database().transaction(|c| { c.execute_batch("CREATE TEMP TRIGGER reject_opaque_hint BEFORE INSERT ON opaque_retained_hints BEGIN SELECT RAISE(ABORT,'injected'); END;")?; Ok(()) }).unwrap();
        cache.remember(mailbox, &[(id(11), inner)]);
        assert_eq!(
            cache.seen(mailbox, &log).unwrap(),
            HashSet::from([id(10), id(11)])
        );
        owner
            .database()
            .transaction(|c| {
                c.execute_batch("DROP TRIGGER reject_opaque_hint")?;
                c.execute("UPDATE opaque_hint_owner SET bytes=?1", [MAX_BYTES as i64])?;
                Ok(())
            })
            .unwrap();
        cache.remember(mailbox, &[(id(12), inner)]);
        assert_eq!(
            cache.seen(mailbox, &log).unwrap(),
            HashSet::from([id(10), id(11), id(12)])
        );
        let fresh = OpaqueReceiveCache::open(
            owner.clone(),
            &path,
            "pw",
            &device.public(),
            &account.account_id(),
        );
        assert_eq!(
            fresh.seen(mailbox, &log).unwrap(),
            HashSet::from([id(10)]),
            "failed durable writes only establish this run's progress"
        );
    }
}
