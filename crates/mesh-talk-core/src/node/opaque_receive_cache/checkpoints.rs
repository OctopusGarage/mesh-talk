//! Optional opaque protocol checkpoints and authenticated wrapper-ID hints.
//! Callers own envelope authentication and exact protocol-key construction.
use super::*;
use rusqlite::OptionalExtension;

const MAX_KEY: usize = 256;
const CHECKPOINTS: Bucket = Bucket {
    table: "opaque_checkpoints",
    entries: 512,
    bytes: 8 * 1024 * 1024,
    value_limit: 16 * 1024,
};
const WRAPPERS: Bucket = Bucket {
    table: "opaque_wrapper_ids",
    entries: 250_000,
    bytes: 64 * 1024 * 1024,
    value_limit: 256,
};
const SCHEMA: &str = "
CREATE TABLE opaque_checkpoints(sequence INTEGER PRIMARY KEY,key BLOB NOT NULL UNIQUE CHECK(length(key) BETWEEN 1 AND 256),value BLOB NOT NULL CHECK(length(value)<=16384));
CREATE TABLE opaque_wrapper_ids(sequence INTEGER PRIMARY KEY,key BLOB NOT NULL UNIQUE CHECK(length(key) BETWEEN 1 AND 256),value BLOB NOT NULL CHECK(length(value)<=256));
CREATE TABLE opaque_checkpoint_usage(bucket TEXT PRIMARY KEY,records INTEGER NOT NULL CHECK(records>=0),bytes INTEGER NOT NULL CHECK(bytes>=0)) WITHOUT ROWID;
";

struct Bucket {
    table: &'static str,
    entries: i64,
    bytes: i64,
    value_limit: usize,
}

pub(super) fn initialize(profile: &ProfileStorage) -> Result<(), LogError> {
    profile.database().transaction(|c| {
        let exists: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='opaque_checkpoint_usage')",
            [],
            |r| r.get(0),
        )?;
        if !exists {
            c.execute_batch(SCHEMA)?;
            for bucket in [&CHECKPOINTS, &WRAPPERS] {
                c.execute(
                    "INSERT INTO opaque_checkpoint_usage VALUES(?1,0,0)",
                    [bucket.table],
                )?;
            }
        }
        crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
        for bucket in [&CHECKPOINTS, &WRAPPERS] {
            bucket.usage(c)?;
        }
        Ok(())
    })
}

impl Bucket {
    fn key(&self, key: &[u8]) -> Result<(), LogError> {
        if key.is_empty() || key.len() > MAX_KEY {
            return Err(invalid("opaque cache key limit"));
        }
        Ok(())
    }
    fn usage(&self, c: &Connection) -> Result<(i64, i64), LogError> {
        let (records, bytes): (i64, i64) = c.query_row(
            "SELECT records,bytes FROM opaque_checkpoint_usage WHERE bucket=?1",
            [self.table],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if !(0..=self.entries).contains(&records) || !(0..=self.bytes).contains(&bytes) {
            return Err(invalid("invalid opaque cache usage"));
        }
        Ok((records, bytes))
    }
    fn get(&self, cache: &OpaqueReceiveCache, key: &[u8]) -> Result<Option<Vec<u8>>, LogError> {
        self.key(key)?;
        let profile = cache.checkpoint_profile()?;
        profile.database().read(|c| {
            let sql = format!("SELECT value FROM {} WHERE key=?1", self.table);
            let mut statement = c.prepare_cached(&sql)?;
            let mut rows = statement.query([key])?;
            let Some(row) = rows.next()? else {
                return Ok(None);
            };
            let value = row
                .get_ref(0)?
                .as_blob()
                .map_err(|_| invalid("invalid opaque cache blob"))?;
            if value.len() > self.value_limit {
                return Err(invalid("opaque cache blob limit"));
            }
            Ok(Some(value.to_vec()))
        })
    }
    fn put(&self, cache: &OpaqueReceiveCache, key: &[u8], value: &[u8]) -> Result<bool, LogError> {
        self.key(key)?;
        if value.len() > self.value_limit {
            return Err(invalid("opaque cache blob limit"));
        }
        let profile = cache.checkpoint_profile()?;
        let unchanged = profile.database().read(|c| {
            c.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {} WHERE key=?1 AND value=?2)",
                    self.table
                ),
                params![key, value],
                |row| row.get::<_, bool>(0),
            )
            .map_err(LogError::from)
        })?;
        if unchanged {
            return Ok(true);
        }
        // Sync before entering the SQL guard. The transaction still checks the
        // exact value again; this preflight does not own concurrent mutation.
        cache.checkpoint_source()?.check_write()?;
        profile.database().transaction(|c| {
            let existing: Option<(i64,bool)> = c.query_row(&format!("SELECT length(key)+length(value),value=?2 FROM {} WHERE key=?1",self.table), params![key,value], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
            // A warmed poll is read-only, including FIFO metadata.
            if matches!(existing,Some((_,true))) { return Ok(true); }
            let (mut records,mut bytes) = self.usage(c)?;
            let charge = (key.len()+value.len()) as i64;
            let previous = existing.map_or(0, |(bytes,_)| bytes);
            if previous < 0 || previous > bytes { return Err(invalid("invalid opaque cache record charge")); }
            bytes -= previous;
            let added = i64::from(existing.is_none());
            while records+added > self.entries || bytes+charge > self.bytes {
                let oldest: Option<(i64,i64)> = c.query_row(&format!("SELECT sequence,length(key)+length(value) FROM {} WHERE key<>?1 ORDER BY sequence LIMIT 1",self.table), [key], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
                let Some((sequence,charged)) = oldest else { return Err(invalid("inconsistent opaque cache capacity")); };
                if records <= 0 || charged < 0 || charged > bytes { return Err(invalid("invalid opaque cache eviction charge")); }
                c.execute(&format!("DELETE FROM {} WHERE sequence=?1",self.table), [sequence])?;
                records -= 1; bytes -= charged;
            }
            if existing.is_some() {
                c.execute(&format!("UPDATE {} SET value=?2 WHERE key=?1",self.table), params![key,value])?;
            } else {
                c.execute(&format!("INSERT INTO {}(key,value) VALUES(?1,?2)",self.table), params![key,value])?;
            }
            c.execute("UPDATE opaque_checkpoint_usage SET records=?2,bytes=?3 WHERE bucket=?1", params![self.table,records+added,bytes+charge])?;
            Ok(true)
        })
    }
    fn remove(&self, cache: &OpaqueReceiveCache, key: &[u8]) -> Result<(), LogError> {
        self.key(key)?;
        let profile = cache.checkpoint_profile()?;
        let exists = profile.database().read(|c| {
            c.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {} WHERE key=?1)", self.table),
                [key],
                |row| row.get::<_, bool>(0),
            )
            .map_err(LogError::from)
        })?;
        if !exists {
            return Ok(());
        }
        cache.checkpoint_source()?.check_write()?;
        profile.database().transaction(|c| {
            let charge: Option<i64> = c
                .query_row(
                    &format!(
                        "SELECT length(key)+length(value) FROM {} WHERE key=?1",
                        self.table
                    ),
                    [key],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(charge) = charge {
                let (records, bytes) = self.usage(c)?;
                if records <= 0 || charge < 0 || charge > bytes {
                    return Err(invalid("invalid opaque cache removal charge"));
                }
                c.execute(&format!("DELETE FROM {} WHERE key=?1", self.table), [key])?;
                c.execute(
                    "UPDATE opaque_checkpoint_usage SET records=?2,bytes=?3 WHERE bucket=?1",
                    params![self.table, records - 1, bytes - charge],
                )?;
            }
            Ok(())
        })
    }
}

impl OpaqueReceiveCache {
    fn checkpoint_source(&self) -> Result<&PublishedHistorySource, LogError> {
        if !self.checkpoints_ready {
            return Err(invalid("opaque checkpoint cache unavailable"));
        }
        self.source
            .as_ref()
            .ok_or_else(|| invalid("opaque checkpoint owner unavailable"))
    }
    fn checkpoint_profile(&self) -> Result<&ProfileStorage, LogError> {
        let source = self.checkpoint_source()?;
        source.check_barrier()?;
        Ok(source.profile())
    }
    pub(in crate::node) fn get_checkpoint(&self, key: &[u8]) -> Result<Option<Vec<u8>>, LogError> {
        CHECKPOINTS.get(self, key)
    }
    pub(in crate::node) fn put_checkpoint(
        &self,
        key: &[u8],
        value: &[u8],
    ) -> Result<bool, LogError> {
        CHECKPOINTS.put(self, key, value)
    }
    pub(in crate::node) fn remove_checkpoint(&self, key: &[u8]) -> Result<(), LogError> {
        CHECKPOINTS.remove(self, key)
    }
    pub(in crate::node) fn get_wrapper_hint(
        &self,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, LogError> {
        WRAPPERS.get(self, key)
    }
    pub(in crate::node) fn put_wrapper_hint(
        &self,
        key: &[u8],
        value: &[u8],
    ) -> Result<bool, LogError> {
        WRAPPERS.put(self, key, value)
    }
    pub(in crate::node) fn session_epoch(&self) -> [u8; 16] {
        self.session
    }
    /// Resolve only this page's IDs; release the SQL guard before retained-log
    /// checks, since the event log can share the same profile connection.
    pub(in crate::node) fn seen_selected(
        &self,
        mailbox: ConversationId,
        ids: &[EventId],
        log: &PersistentEventLog,
    ) -> Result<HashSet<EventId>, LogError> {
        self.selected_hints(mailbox, ids, log, true)
    }

    pub(in crate::node) fn retained_seen_selected(
        &self,
        mailbox: ConversationId,
        ids: &[EventId],
        log: &PersistentEventLog,
    ) -> Result<HashSet<EventId>, LogError> {
        self.selected_hints(mailbox, ids, log, false)
    }

    fn selected_hints(
        &self,
        mailbox: ConversationId,
        ids: &[EventId],
        log: &PersistentEventLog,
        include_session: bool,
    ) -> Result<HashSet<EventId>, LogError> {
        if ids.len() > 1500 {
            return Err(invalid("opaque selected hint page limit"));
        }
        let Some(profile) = &self.profile else {
            return Ok(HashSet::new());
        };
        let candidates = profile.database().read(|c| {
            let mut session = if include_session {
                Some(c.prepare_cached("SELECT inner_id FROM opaque_session_hints WHERE mailbox=?1 AND outer_id=?2 AND session=?3")?)
            } else { None };
            let mut retained = if self.source.is_some() {
                Some(c.prepare_cached("SELECT inner_id FROM opaque_retained_hints WHERE mailbox=?1 AND outer_id=?2")?)
            } else { None };
            let mut result = Vec::with_capacity(ids.len());
            for outer in ids {
                let session_inner = if let Some(statement) = &mut session {
                    let mut rows = statement.query(params![mailbox.as_bytes().as_slice(),outer.as_bytes().as_slice(),self.session.as_slice()])?;
                    match rows.next()? {
                        Some(row) if matches!(row.get_ref(0)?, rusqlite::types::ValueRef::Null) => Some(None),
                        Some(row) => Some(Some(id(row,0)?)),
                        None => None,
                    }
                } else { None };
                let retained_inner = if let Some(statement) = &mut retained {
                    let mut rows = statement.query(params![mailbox.as_bytes().as_slice(),outer.as_bytes().as_slice()])?;
                    rows.next()?.map(|row|id(row,0)).transpose()?
                } else { None };
                if session_inner.is_some() || retained_inner.is_some() {
                    result.push((*outer, session_inner, retained_inner));
                }
            }
            Ok(result)
        })?;
        let mut seen = HashSet::with_capacity(candidates.len());
        for (outer, session_inner, retained_inner) in candidates {
            let session_seen = match session_inner {
                Some(None) => true,
                Some(Some(inner)) => log.has(&inner)?,
                None => false,
            };
            if session_seen
                || match retained_inner {
                    Some(inner) => log.has(&inner)?,
                    None => false,
                }
            {
                seen.insert(outer);
            }
        }
        Ok(seen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{account::Account, device::DeviceIdentity};
    use crate::storage::profile::ProfileStorageLease;

    fn fixture() -> (
        tempfile::TempDir,
        Arc<ProfileStorage>,
        DeviceIdentity,
        Account,
        OpaqueReceiveCache,
    ) {
        let root = tempfile::tempdir().unwrap();
        let device = DeviceIdentity::generate();
        let account = Account::generate();
        let profile = ProfileStorage::open_participant(
            ProfileStorageLease::acquire(root.path()).unwrap(),
            &device.public(),
            &account.public(),
            &root.path().join("events"),
            &root.path().join("sent"),
            "pw",
        )
        .unwrap();
        profile.complete_participant("pw", None).unwrap();
        let cache = OpaqueReceiveCache::open(
            profile.clone(),
            &root.path().join("events"),
            "pw",
            &device.public(),
            &account.account_id(),
        );
        (root, profile, device, account, cache)
    }

    fn retained_pairs() -> Vec<(EventId, EventId)> {
        (0_u64..130)
            .map(|number| {
                let mut bytes = [0; 32];
                bytes[..8].copy_from_slice(&number.to_be_bytes());
                (EventId::new(bytes), EventId::new([9; 32]))
            })
            .collect()
    }

    fn assert_quiet_io(operation: impl FnOnce(&mut OpaqueReceiveCache)) {
        let (_root, profile, _device, _account, mut cache) = fixture();
        cache.put_checkpoint(b"checkpoint", b"sealed").unwrap();
        cache.put_wrapper_hint(b"wrapper", b"outer").unwrap();
        cache.remember(ConversationId::new([1; 32]), &retained_pairs());
        let changes = profile.database().read(|c| Ok(c.total_changes())).unwrap();
        crate::storage::profile::publication::tests::PARENT_SYNCS.with(|value| value.set(0));
        operation(&mut cache);
        assert_eq!(
            profile.database().read(|c| Ok(c.total_changes())).unwrap(),
            changes,
            "unchanged optional cache operations must not mutate SQL"
        );
        assert_eq!(
            crate::storage::profile::publication::tests::PARENT_SYNCS.with(|value| value.get()),
            0,
            "unchanged optional cache operations must not synchronize the parent directory"
        );
    }

    #[test]
    fn quiet_checkpoint_and_wrapper_reads_do_not_sync_parent() {
        assert_quiet_io(|cache| {
            assert_eq!(
                cache.get_checkpoint(b"checkpoint").unwrap(),
                Some(b"sealed".to_vec())
            );
            assert_eq!(cache.get_checkpoint(b"absent").unwrap(), None);
            assert_eq!(
                cache.get_wrapper_hint(b"wrapper").unwrap(),
                Some(b"outer".to_vec())
            );
            assert_eq!(cache.get_wrapper_hint(b"absent").unwrap(), None);
        });
    }

    #[test]
    fn quiet_duplicate_checkpoint_and_wrapper_puts_do_not_sync_parent() {
        assert_quiet_io(|cache| {
            assert!(cache.put_checkpoint(b"checkpoint", b"sealed").unwrap());
            assert!(cache.put_wrapper_hint(b"wrapper", b"outer").unwrap());
        });
    }

    #[test]
    fn quiet_missing_checkpoint_remove_does_not_sync_parent() {
        assert_quiet_io(|cache| cache.remove_checkpoint(b"absent").unwrap());
    }

    #[test]
    fn quiet_empty_receive_hints_do_not_sync_parent() {
        assert_quiet_io(|cache| cache.remember(ConversationId::new([1; 32]), &[]));
    }

    #[test]
    fn quiet_duplicate_bounded_receive_hints_do_not_sync_parent() {
        assert_quiet_io(|cache| cache.remember(ConversationId::new([1; 32]), &retained_pairs()));
    }

    #[test]
    fn changed_cache_write_syncs_and_failed_sync_remains_retryable() {
        let (_root, profile, _device, _account, cache) = fixture();
        cache.put_checkpoint(b"checkpoint", b"old").unwrap();
        let changes = profile.database().read(|c| Ok(c.total_changes())).unwrap();
        crate::storage::profile::publication::tests::PARENT_SYNCS.with(|value| value.set(0));
        crate::storage::profile::publication::tests::CUT
            .with(|value| *value.borrow_mut() = Some("parent-sync"));
        let rejected = cache.put_checkpoint(b"checkpoint", b"new");
        crate::storage::profile::publication::tests::CUT.with(|value| *value.borrow_mut() = None);
        assert!(rejected.is_err());
        assert_eq!(
            profile.database().read(|c| Ok(c.total_changes())).unwrap(),
            changes,
            "a failed barrier sync must precede every cache mutation"
        );
        assert_eq!(
            cache.get_checkpoint(b"checkpoint").unwrap(),
            Some(b"old".to_vec())
        );
        crate::storage::profile::publication::tests::PARENT_SYNCS.with(|value| value.set(0));
        assert!(cache.put_checkpoint(b"checkpoint", b"new").unwrap());
        assert!(
            crate::storage::profile::publication::tests::PARENT_SYNCS.with(|value| value.get()) > 0,
            "a changed write must retry barrier synchronization after failure"
        );
        assert!(profile.database().read(|c| Ok(c.total_changes())).unwrap() > changes);
    }

    #[test]
    fn changed_barrier_rejects_reads_and_noops_before_sql_or_directory_sync() {
        for replace in [false, true] {
            let (root, profile, _device, _account, cache) = fixture();
            cache.put_checkpoint(b"checkpoint", b"sealed").unwrap();
            cache.put_wrapper_hint(b"wrapper", b"outer").unwrap();
            let path = OpaqueReceiveCache::path(&root.path().join("events"));
            if replace {
                let foreign = root.path().join("foreign-stub");
                std::fs::write(&foreign, [3; 54]).unwrap();
                std::fs::rename(foreign, &path).unwrap();
            } else {
                std::fs::write(&path, [4; 54]).unwrap();
            }
            let changes = profile.database().read(|c| Ok(c.total_changes())).unwrap();
            crate::storage::profile::publication::tests::PARENT_SYNCS.with(|value| value.set(0));
            assert!(cache.get_checkpoint(b"checkpoint").is_err());
            assert!(cache.get_checkpoint(b"absent").is_err());
            assert!(cache.get_wrapper_hint(b"wrapper").is_err());
            assert!(cache.put_checkpoint(b"checkpoint", b"sealed").is_err());
            assert!(cache.put_wrapper_hint(b"wrapper", b"outer").is_err());
            assert!(cache.remove_checkpoint(b"absent").is_err());
            assert_eq!(
                profile.database().read(|c| Ok(c.total_changes())).unwrap(),
                changes
            );
            assert_eq!(
                crate::storage::profile::publication::tests::PARENT_SYNCS.with(|value| value.get()),
                0
            );
        }
    }

    #[test]
    fn checkpoint_roundtrip_restart_and_remove() {
        let (root, profile, device, account, cache) = fixture();
        assert!(
            cache
                .put_checkpoint(b"exact-key", b"sealed-checkpoint")
                .unwrap()
        );
        assert_eq!(
            cache.get_checkpoint(b"exact-key").unwrap(),
            Some(b"sealed-checkpoint".to_vec())
        );
        assert_eq!(cache.get_checkpoint(b"other-key").unwrap(), None);
        let epoch = cache.session_epoch();
        drop(cache);
        let cache = OpaqueReceiveCache::open(
            profile,
            &root.path().join("events"),
            "pw",
            &device.public(),
            &account.account_id(),
        );
        assert_eq!(
            cache.get_checkpoint(b"exact-key").unwrap(),
            Some(b"sealed-checkpoint".to_vec())
        );
        assert_ne!(cache.session_epoch(), epoch);
        cache.remove_checkpoint(b"exact-key").unwrap();
        assert_eq!(cache.get_checkpoint(b"exact-key").unwrap(), None);
    }

    #[test]
    fn checkpoint_limits_fifo_and_noop_write() {
        let (_root, profile, _device, _account, cache) = fixture();
        assert!(cache.put_checkpoint(&[0; 257], b"x").is_err());
        assert!(cache.put_checkpoint(b"x", &[0; 16385]).is_err());
        for n in 0u64..512 {
            assert!(cache.put_checkpoint(&n.to_be_bytes(), b"v").unwrap());
        }
        let changes = profile.database().read(|c| Ok(c.total_changes())).unwrap();
        assert!(cache.put_checkpoint(&0u64.to_be_bytes(), b"v").unwrap());
        assert_eq!(
            cache.get_checkpoint(&0u64.to_be_bytes()).unwrap(),
            Some(b"v".to_vec())
        );
        assert_eq!(
            profile.database().read(|c| Ok(c.total_changes())).unwrap(),
            changes
        );
        assert!(cache.put_checkpoint(&512u64.to_be_bytes(), b"v").unwrap());
        assert_eq!(cache.get_checkpoint(&0u64.to_be_bytes()).unwrap(), None);
        assert_eq!(
            cache.get_checkpoint(&1u64.to_be_bytes()).unwrap(),
            Some(b"v".to_vec())
        );
    }

    #[test]
    fn checkpoint_byte_eviction_and_wrapper_budget_are_independent() {
        let (_root, _profile, _device, _account, cache) = fixture();
        let value = vec![7; 16384];
        for n in 0u64..512 {
            assert!(cache.put_checkpoint(&n.to_be_bytes(), &value).unwrap());
        }
        assert_eq!(
            cache.get_checkpoint(&0u64.to_be_bytes()).unwrap(),
            None,
            "key bytes count against the 8 MiB budget"
        );
        assert!(
            cache
                .put_wrapper_hint(b"mailbox-inner", b"sealed-outer-id")
                .unwrap()
        );
        assert_eq!(
            cache.get_wrapper_hint(b"mailbox-inner").unwrap(),
            Some(b"sealed-outer-id".to_vec())
        );
        assert_eq!(cache.get_checkpoint(b"mailbox-inner").unwrap(), None);
    }

    #[test]
    fn foreign_and_changed_source_are_unavailable() {
        let (root, profile, device, _, cache) = fixture();
        assert!(cache.put_checkpoint(b"key", b"sealed").unwrap());
        let foreign = OpaqueReceiveCache::open(
            profile,
            &root.path().join("events"),
            "pw",
            &device.public(),
            "foreign",
        );
        assert!(foreign.get_checkpoint(b"key").is_err());
        std::fs::write(
            OpaqueReceiveCache::path(&root.path().join("events")),
            b"changed",
        )
        .unwrap();
        assert!(cache.get_checkpoint(b"key").is_err());
        assert!(cache.put_checkpoint(b"key", b"new").is_err());
    }
    #[test]
    fn incompatible_checkpoint_extension_preserves_login_and_old_hints() {
        let (root, profile, device, account, cache) = fixture();
        assert!(cache.put_checkpoint(b"key", b"sealed").unwrap());
        profile.database().transaction(|c| {
            c.execute_batch("DROP TABLE opaque_checkpoints; CREATE TABLE opaque_checkpoints(unrelated INTEGER)")?;
            Ok(())
        }).unwrap();
        drop(cache);
        let cache = OpaqueReceiveCache::open(
            profile,
            &root.path().join("events"),
            "pw",
            &device.public(),
            &account.account_id(),
        );
        assert!(
            cache.source.is_some(),
            "optional extension cannot disable the older receive owner"
        );
        assert!(cache.get_checkpoint(b"key").is_err());
    }

    #[test]
    fn selected_hints_match_legacy_seen_without_enumerating_other_mailboxes() {
        let (_root, profile, device, _account, mut cache) = fixture();
        let mut log = PersistentEventLog::open_profile(profile, "pw").unwrap();
        let mailbox = ConversationId::new([1; 32]);
        let event = crate::eventlog::Event::new(
            &device,
            mailbox,
            1,
            vec![],
            1,
            0,
            crate::eventlog::EventKind::Message,
            vec![7],
        );
        let inner = event.id;
        let first = EventId::new([2; 32]);
        let control = EventId::new([3; 32]);
        let missing = EventId::new([4; 32]);
        log.append_durable(event).unwrap();
        cache.remember(mailbox, &[(first, inner), (missing, EventId::new([5; 32]))]);
        cache.remember_session(mailbox, &[control]);
        let selected = [first, control, missing];
        assert_eq!(
            cache
                .retained_seen_selected(mailbox, &selected, &log)
                .unwrap(),
            HashSet::from([first])
        );

        assert_eq!(
            cache.seen_selected(mailbox, &selected, &log).unwrap(),
            cache.seen(mailbox, &log).unwrap()
        );
        assert!(cache.seen_selected(mailbox, &[first; 1501], &log).is_err());
        log.drop_conversation(&mailbox).unwrap();
        assert_eq!(
            cache.seen_selected(mailbox, &selected, &log).unwrap(),
            HashSet::from([control])
        );
    }
}
