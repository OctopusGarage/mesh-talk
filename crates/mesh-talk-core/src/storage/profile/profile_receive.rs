//! Exact profile receive discovery and retry facts, independent of UI delivery.
use crate::{
    eventlog::{
        LogError,
        event::{Author, ConversationId, EventId},
        repository::CommittedHeader,
    },
    storage::profile::context::ProfileStorage,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::Arc;
const SCHEMA: &str = "CREATE TABLE profile_receive_scan(account BLOB PRIMARY KEY CHECK(length(account)=32), after_commit INTEGER CHECK(after_commit IS NULL OR after_commit>=0), next_ticket INTEGER NOT NULL CHECK(next_ticket>0));
CREATE TABLE profile_receive_pending(account BLOB NOT NULL CHECK(length(account)=32), id BLOB NOT NULL CHECK(length(id)=32), conversation BLOB NOT NULL CHECK(length(conversation)=32), ticket INTEGER NOT NULL CHECK(ticket>0), PRIMARY KEY(account,id));
CREATE INDEX profile_receive_retry ON profile_receive_pending(account,ticket);
CREATE TABLE profile_receive_processed(account BLOB NOT NULL CHECK(length(account)=32), id BLOB NOT NULL CHECK(length(id)=32), PRIMARY KEY(account,id));";
/// Shared by receive-work startup and authorized source retirement. The exact
/// processed fact means presentation terminal or superseded-source retirement;
/// it never certifies retained custody or a newly admitted event.
pub(crate) fn ensure_schema_on(c: &Connection) -> Result<(), LogError> {
    let present: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='profile_receive_scan')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        c.execute_batch(SCHEMA)?;
    }
    super::manifest::validate_ddl(c, SCHEMA)
}

impl ProfileStorage {
    /// Called once before a maintenance deletion loop. Nonparticipant domains
    /// have no account-scoped presentation work to retire.
    pub(crate) fn ensure_profile_receive_on(
        &self,
        c: &Connection,
        domain: crate::eventlog::repository::EventDomainId,
    ) -> Result<bool, LogError> {
        if domain.0 != *b"MeshPartEventsV1" {
            return Ok(false);
        }
        self.check_participant_account_on(c)?;
        ensure_schema_on(c)?;
        Ok(true)
    }

    /// Only authorized profile compaction calls this, immediately before the
    /// selected source deletion in the same transaction. Attribution comes from
    /// the source's committed ownership interval, never the current account.
    pub(crate) fn record_compacted_on(
        &self,
        c: &Connection,
        domain: crate::eventlog::repository::EventDomainId,
        id: EventId,
        commit_ordinal: i64,
    ) -> Result<(), LogError> {
        if domain.0 != *b"MeshPartEventsV1" {
            return Ok(());
        }
        self.check_participant_account_on(c)?;
        let namespace =
            super::participant::event_ownership::participant_event_namespace_on(c, commit_ordinal)?;
        let account: Option<Vec<u8>> = c
            .query_row(
                "SELECT account FROM participant_account_namespaces WHERE account_id=?1",
                [namespace],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        // Legacy namespaces may have no authenticated public account. Do not
        // invent that missing identity from the current owner or a short ID.
        let Some(account) = account else {
            return Ok(());
        };
        if account.len() != 32 {
            return Err(invalid("invalid profile retirement account"));
        }
        c.execute(
            "INSERT OR IGNORE INTO profile_receive_processed VALUES(?1,?2)",
            params![account.as_slice(), id.as_bytes().as_slice()],
        )?;
        c.execute(
            "DELETE FROM profile_receive_pending WHERE account=?1 AND id=?2",
            params![account.as_slice(), id.as_bytes().as_slice()],
        )?;
        Ok(())
    }
}
fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}
fn active(c: &Connection) -> Result<[u8; 32], LogError> {
    c.query_row(
        "SELECT account FROM participant_profile WHERE singleton=1",
        [],
        |r| r.get::<_, Vec<u8>>(0),
    )?
    .try_into()
    .map_err(|_| invalid("invalid profile receive account"))
}
pub(crate) struct ProfileReceiveWork {
    profile: Arc<ProfileStorage>,
    account: [u8; 32],
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub id: EventId,
    pub conversation: ConversationId,
}
impl ProfileReceiveWork {
    pub fn open(profile: Arc<ProfileStorage>) -> Result<Self, LogError> {
        let account = profile.database().transaction(|c| {
            ensure_schema_on(c)?;
            let account = active(c)?;
            c.execute(
                "INSERT OR IGNORE INTO profile_receive_scan VALUES(?1,NULL,1)",
                [account.as_slice()],
            )?;
            Ok(account)
        })?;
        Ok(Self { profile, account })
    }
    fn check(&self, c: &Connection) -> Result<(), LogError> {
        if active(c)? != self.account {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }
    pub fn after(&self) -> Result<Option<i64>, LogError> {
        self.profile.database().read(|c| {
            self.check(c)?;
            Ok(c.query_row(
                "SELECT after_commit FROM profile_receive_scan WHERE account=?1",
                [self.account.as_slice()],
                |r| r.get(0),
            )?)
        })
    }
    /// Exact account-scoped presentation terminal or authorized superseded-source
    /// retirement fact; never retained custody or new event admission.
    pub fn processed(&self, id: EventId) -> Result<bool, LogError> {
        self.profile.database().read(|c| {
            self.check(c)?;
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM profile_receive_processed WHERE account=?1 AND id=?2)",
                params![self.account.as_slice(), id.as_bytes().as_slice()],
                |r| r.get(0),
            )?)
        })
    }
    pub fn discover(
        &self,
        after: Option<i64>,
        headers: &[CommittedHeader],
        own: Author,
    ) -> Result<(), LogError> {
        if headers.len() > 128
            || after.is_some_and(|n| n < 0)
            || headers
                .windows(2)
                .any(|pair| pair[0].ordinal >= pair[1].ordinal)
            || headers
                .first()
                .is_some_and(|row| row.ordinal <= after.unwrap_or(-1))
        {
            return Err(LogError::InvalidPageBudget);
        }
        self.profile.database().transaction(|c| {
            self.check(c)?;
            let (current,mut ticket):(Option<i64>,i64)=c.query_row("SELECT after_commit,next_ticket FROM profile_receive_scan WHERE account=?1",[self.account.as_slice()],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if current!=after {return Err(LogError::StaleCursor);}
            for row in headers {
                if !row.active || row.header.kind!=crate::eventlog::event::EventKind::Profile || row.header.author==own {continue;}
                let inserted=c.execute("INSERT OR IGNORE INTO profile_receive_pending SELECT ?1,?2,?3,?4 WHERE NOT EXISTS(SELECT 1 FROM profile_receive_processed WHERE account=?1 AND id=?2)",params![self.account.as_slice(),row.header.id.as_bytes().as_slice(),row.header.conversation.as_bytes().as_slice(),ticket])?;
                if inserted!=0 {ticket=ticket.checked_add(1).ok_or_else(||invalid("profile retry ticket exhausted"))?;}
            }
            if let Some(last)=headers.last() {c.execute("UPDATE profile_receive_scan SET after_commit=?1,next_ticket=?2 WHERE account=?3",params![last.ordinal,ticket,self.account.as_slice()])?;}
            Ok(())
        })
    }
    /// Rotate a fixed metadata page before expensive authentication. A storage
    /// or decryption failure leaves exact pending work eligible on later passes.
    pub fn candidates(&self) -> Result<Vec<Candidate>, LogError> {
        self.profile.database().transaction(|c| {
            self.check(c)?;
            let mut result=Vec::new();
            let mut statement=c.prepare("SELECT id,conversation FROM profile_receive_pending WHERE account=?1 ORDER BY ticket LIMIT 16")?;
            let mut rows=statement.query([self.account.as_slice()])?;
            while let Some(row)=rows.next()? {
                let id:Vec<u8>=row.get(0)?;let conversation:Vec<u8>=row.get(1)?;
                result.push(Candidate {id:EventId::new(id.try_into().map_err(|_|invalid("invalid profile retry id"))?),conversation:ConversationId::new(conversation.try_into().map_err(|_|invalid("invalid profile retry conversation"))?)});
            }
            drop(rows);drop(statement);
            let mut ticket:i64=c.query_row("SELECT next_ticket FROM profile_receive_scan WHERE account=?1",[self.account.as_slice()],|r|r.get(0))?;
            for row in &result {
                c.execute("UPDATE profile_receive_pending SET ticket=?1 WHERE account=?2 AND id=?3",params![ticket,self.account.as_slice(),row.id.as_bytes().as_slice()])?;
                ticket=ticket.checked_add(1).ok_or_else(||invalid("profile retry ticket exhausted"))?;
            }
            c.execute("UPDATE profile_receive_scan SET next_ticket=?1 WHERE account=?2",params![ticket,self.account.as_slice()])?;
            Ok(result)
        })
    }
    /// Call only after durable current-profile installation or definitive
    /// authenticated rejection. There is no second authoritative payload WAL.
    pub fn complete(&self, id: EventId) -> Result<(), LogError> {
        self.profile.database().transaction(|c| {
            self.check(c)?;
            c.execute(
                "INSERT OR IGNORE INTO profile_receive_processed VALUES(?1,?2)",
                params![self.account.as_slice(), id.as_bytes().as_slice()],
            )?;
            c.execute(
                "DELETE FROM profile_receive_pending WHERE account=?1 AND id=?2",
                params![self.account.as_slice(), id.as_bytes().as_slice()],
            )?;
            Ok(())
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        eventlog::{event::EventKind, repository::EventHeader},
        identity::{account::Account, device::DeviceIdentity},
        storage::profile::ProfileStorageLease,
    };
    fn fixture() -> (tempfile::TempDir, ProfileReceiveWork, Author) {
        let dir = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let profile = ProfileStorage::open_participant(
            ProfileStorageLease::acquire(dir.path()).unwrap(),
            &identity.public(),
            &account.public(),
            &dir.path().join("events"),
            &dir.path().join("sent"),
            "pw",
        )
        .unwrap();
        profile.complete_participant("pw", None).unwrap();
        (
            dir,
            ProfileReceiveWork::open(profile).unwrap(),
            Author::from_ed25519(identity.public().ed25519_pub),
        )
    }
    fn headers() -> Vec<CommittedHeader> {
        let author = Author::from_ed25519(DeviceIdentity::generate().public().ed25519_pub);
        (1..=80)
            .map(|n| CommittedHeader {
                ordinal: n,
                active: true,
                header: EventHeader {
                    id: EventId::new([n as u8; 32]),
                    conversation: ConversationId::new([1; 32]),
                    author,
                    seq: n as u64,
                    lamport: n as u64,
                    wall_clock: n as u64,
                    kind: EventKind::Profile,
                    parent_count: 0,
                    encoded_bytes: 128,
                    ciphertext_bytes: 64,
                    signature_bytes: 64,
                    stored_bytes: 256,
                },
            })
            .collect()
    }
    #[test]
    fn bounded_retry_selection_rotates_and_exact_completion_survives_reopen() {
        let (_dir, work, own) = fixture();
        work.discover(None, &headers(), own).unwrap();
        assert_eq!(work.after().unwrap(), Some(80));
        let first = work.candidates().unwrap();
        assert_eq!(first.len(), 16);
        work.complete(first[0].id).unwrap();
        let profile = work.profile.clone();
        drop(work);
        let work = ProfileReceiveWork::open(profile).unwrap();
        let second = work.candidates().unwrap();
        assert_eq!(second.len(), 16);
        assert!(second.iter().all(|row| !first.contains(row)));
        let mut seen = std::collections::HashSet::new();
        for _ in 0..5 {
            seen.extend(work.candidates().unwrap().into_iter().map(|row| row.id));
        }
        assert_eq!(seen.len(), 79);
        assert!(!seen.contains(&first[0].id));
    }
    #[test]
    fn exact_processed_fact_survives_same_account_reopen_without_matching_other_ids() {
        let (_dir, work, own) = fixture();
        work.discover(None, &headers(), own).unwrap();
        let id = EventId::new([1; 32]);
        let other = EventId::new([2; 32]);
        assert!(!work.processed(id).unwrap());
        work.complete(id).unwrap();
        assert!(work.processed(id).unwrap());
        assert!(!work.processed(other).unwrap());
        let profile = work.profile.clone();
        drop(work);
        let work = ProfileReceiveWork::open(profile).unwrap();
        assert!(work.processed(id).unwrap());
        assert!(!work.processed(other).unwrap());
    }
    #[test]
    fn discovery_failure_rolls_back_cursor_and_pending_and_retries_exactly() {
        let (_dir, work, own) = fixture();
        work.profile.database().transaction(|c| {
            c.execute_batch("CREATE TRIGGER fail_profile_discovery BEFORE INSERT ON profile_receive_pending WHEN NEW.ticket=20 BEGIN SELECT RAISE(ABORT,'injected profile discovery failure'); END;")?;
            Ok(())
        }).unwrap();
        assert!(work.discover(None, &headers(), own).is_err());
        assert_eq!(work.after().unwrap(), None);
        assert!(work.candidates().unwrap().is_empty());
        work.profile
            .database()
            .transaction(|c| {
                c.execute_batch("DROP TRIGGER fail_profile_discovery")?;
                Ok(())
            })
            .unwrap();
        work.discover(None, &headers(), own).unwrap();
        assert_eq!(work.after().unwrap(), Some(80));
        assert_eq!(work.candidates().unwrap().len(), 16);
        assert!(matches!(
            work.discover(None, &headers(), own),
            Err(LogError::StaleCursor)
        ));
    }
    #[test]
    fn retired_and_own_metadata_advance_discovery_without_scheduling_bodies() {
        let (_dir, work, own) = fixture();
        let mut page = headers();
        for (n, row) in page.iter_mut().enumerate() {
            if n % 2 == 0 {
                row.active = false;
            } else {
                row.header.author = own;
            }
        }
        work.discover(None, &page, own).unwrap();
        assert_eq!(work.after().unwrap(), Some(80));
        assert!(work.candidates().unwrap().is_empty());
    }
    #[test]
    fn account_authority_change_fences_old_work_and_keeps_prior_pending_namespace() {
        let (_dir, work, own) = fixture();
        work.discover(None, &headers(), own).unwrap();
        let account = Account::generate();
        work.profile
            .database()
            .transaction(|c| {
                c.execute(
                    "UPDATE participant_profile SET account=?1 WHERE singleton=1",
                    [account.public().ed25519_pub.as_slice()],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(matches!(work.after(), Err(LogError::StaleCursor)));
        assert!(matches!(
            work.processed(EventId::new([1; 32])),
            Err(LogError::StaleCursor)
        ));
        assert!(matches!(
            work.complete(EventId::new([1; 32])),
            Err(LogError::StaleCursor)
        ));
        let replacement = ProfileReceiveWork::open(work.profile.clone()).unwrap();
        assert_eq!(replacement.after().unwrap(), None);
        assert!(replacement.candidates().unwrap().is_empty());
        let retained: i64 = work
            .profile
            .database()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM profile_receive_pending WHERE account=?1",
                    [work.account.as_slice()],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(retained, 80);
    }
    const DOMAIN: crate::eventlog::repository::EventDomainId =
        crate::eventlog::repository::EventDomainId(*b"MeshPartEventsV1");

    fn open_storage(
        dir: &tempfile::TempDir,
        identity: &DeviceIdentity,
        account: &Account,
    ) -> Arc<ProfileStorage> {
        let profile = ProfileStorage::open_participant(
            ProfileStorageLease::acquire(dir.path()).unwrap(),
            &identity.public(),
            &account.public(),
            &dir.path().join("events"),
            &dir.path().join("sent"),
            "pw",
        )
        .unwrap();
        profile.complete_participant("pw", None).unwrap();
        drop(crate::eventlog::PersistentEventLog::open_profile(profile.clone(), "pw").unwrap());
        profile
    }

    fn append_profiles(repo: &crate::eventlog::repository::EventRepository) -> Vec<EventId> {
        let author = DeviceIdentity::generate();
        (1..=12)
            .map(|seq| {
                let event = crate::eventlog::Event::new(
                    &author,
                    ConversationId::new([1; 32]),
                    seq,
                    vec![],
                    seq,
                    seq,
                    EventKind::Profile,
                    vec![42; 128],
                );
                repo.append(&event).unwrap();
                event.id
            })
            .collect()
    }

    fn prepare_compaction(
        repo: &crate::eventlog::repository::EventRepository,
    ) -> crate::eventlog::repository::maintenance::ProfileMaintenanceToken {
        use crate::eventlog::repository::{maintenance::MaintenanceState, pages::PageBudget};
        repo.request_profile_maintenance(&ConversationId::new([1; 32]))
            .unwrap();
        let token = repo.begin_profile_maintenance().unwrap();
        for _ in 0..100 {
            if repo
                .step_profile_maintenance(
                    token,
                    PageBudget {
                        rows: 16,
                        bytes: 8192,
                    },
                )
                .unwrap()
                .state
                == MaintenanceState::Ready
            {
                return token;
            }
        }
        panic!("profile compaction did not finish");
    }

    fn fact_count(profile: &ProfileStorage, account: &[u8; 32]) -> i64 {
        profile
            .database()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM profile_receive_processed WHERE account=?1",
                    [account.as_slice()],
                    |r| r.get(0),
                )?)
            })
            .unwrap()
    }

    #[test]
    fn compaction_before_receive_work_open_persists_exact_retirement_across_reopen() {
        use crate::eventlog::repository::EventRepository;
        let dir = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let profile = open_storage(&dir, &identity, &account);
        let repo = EventRepository::from_profile(profile.clone(), DOMAIN).unwrap();
        let ids = append_profiles(&repo);
        let token = prepare_compaction(&repo);
        assert_eq!(repo.apply_profile_maintenance(token).unwrap().dropped, 10);
        assert_eq!(repo.body_read_counts(), (0, 0));
        let work = ProfileReceiveWork::open(profile.clone()).unwrap();
        for id in &ids[..10] {
            assert!(!repo.has(id).unwrap());
            assert!(work.processed(*id).unwrap());
        }
        for id in &ids[10..] {
            assert!(!work.processed(*id).unwrap());
        }
        drop(work);
        drop(repo);
        drop(profile);
        let profile = open_storage(&dir, &identity, &account);
        let work = ProfileReceiveWork::open(profile).unwrap();
        for id in &ids[..10] {
            assert!(work.processed(*id).unwrap());
        }
        assert!(!work.processed(EventId::new([99; 32])).unwrap());
    }

    #[test]
    fn deletion_failure_rolls_back_retirement_and_exact_pending_removal() {
        use crate::eventlog::repository::EventRepository;
        let dir = tempfile::tempdir().unwrap();
        let profile = open_storage(&dir, &DeviceIdentity::generate(), &Account::generate());
        let repo = EventRepository::from_profile(profile.clone(), DOMAIN).unwrap();
        let ids = append_profiles(&repo);
        let work = ProfileReceiveWork::open(profile.clone()).unwrap();
        profile.database().transaction(|c| {
            for (n,id) in ids.iter().enumerate() {
                c.execute("INSERT INTO profile_receive_pending VALUES(?1,?2,?3,?4)",
                    params![work.account.as_slice(),id.as_bytes().as_slice(),[1u8;32].as_slice(),n as i64+1])?;
            }
            // The delete must observe its just-written retirement fact. Raising
            // here tests rollback across the actual fact + selected-delete seam.
            c.execute_batch("CREATE TEMP TRIGGER fail_retired_delete BEFORE DELETE ON retained_events WHEN EXISTS(SELECT 1 FROM profile_receive_processed WHERE id=OLD.id) BEGIN SELECT RAISE(ABORT,'injected selected deletion failure'); END;")?;
            Ok(())
        }).unwrap();
        let token = prepare_compaction(&repo);
        assert!(repo.apply_profile_maintenance(token).is_err());
        assert_eq!(fact_count(&profile, &work.account), 0);
        for id in &ids {
            assert!(repo.has(id).unwrap());
        }
        let pending = || {
            profile
                .database()
                .read(|c| {
                    Ok(
                        c.query_row("SELECT count(*) FROM profile_receive_pending", [], |r| {
                            r.get::<_, i64>(0)
                        })?,
                    )
                })
                .unwrap()
        };
        assert_eq!(pending(), 12);
        profile
            .database()
            .transaction(|c| {
                c.execute_batch("DROP TRIGGER fail_retired_delete")?;
                Ok(())
            })
            .unwrap();
        assert_eq!(repo.apply_profile_maintenance(token).unwrap().dropped, 10);
        assert_eq!(pending(), 2);
        assert_eq!(fact_count(&profile, &work.account), 10);
    }

    #[test]
    fn adoption_compaction_attributes_old_sources_to_a_and_fences_stale_context() {
        use crate::{
            eventlog::repository::EventRepository,
            storage::profile::participant::{finish_adoption, prepare_adoption},
        };
        let dir = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let a = Account::generate();
        let b = Account::generate();
        let profile = open_storage(&dir, &identity, &a);
        let repo = EventRepository::from_profile(profile.clone(), DOMAIN).unwrap();
        let ids = append_profiles(&repo);
        let work = ProfileReceiveWork::open(profile.clone()).unwrap();
        profile
            .database()
            .transaction(|c| {
                c.execute(
                    "INSERT INTO profile_receive_pending VALUES(?1,?2,?3,1)",
                    params![
                        a.public().ed25519_pub.as_slice(),
                        ids[0].as_bytes().as_slice(),
                        [1u8; 32].as_slice()
                    ],
                )?;
                Ok(())
            })
            .unwrap();
        prepare_adoption(profile.database(), &a.public(), &b.public()).unwrap();
        finish_adoption(profile.database(), &b.public()).unwrap();
        profile
            .database()
            .transaction(|c| {
                c.execute(
                    "INSERT INTO profile_receive_pending VALUES(?1,?2,?3,1)",
                    params![
                        b.public().ed25519_pub.as_slice(),
                        ids[0].as_bytes().as_slice(),
                        [1u8; 32].as_slice()
                    ],
                )?;
                Ok(())
            })
            .unwrap();
        let token = prepare_compaction(&repo);
        assert!(matches!(
            repo.apply_profile_maintenance(token),
            Err(LogError::StaleCursor)
        ));
        for id in &ids {
            assert!(repo.has(id).unwrap());
        }
        assert_eq!(fact_count(&profile, &a.public().ed25519_pub), 0);
        drop(work);
        drop(repo);
        drop(profile);
        let profile = open_storage(&dir, &identity, &b);
        let repo = EventRepository::from_profile(profile.clone(), DOMAIN).unwrap();
        let token = prepare_compaction(&repo);
        assert_eq!(repo.apply_profile_maintenance(token).unwrap().dropped, 10);
        assert_eq!(fact_count(&profile, &a.public().ed25519_pub), 10);
        assert_eq!(fact_count(&profile, &b.public().ed25519_pub), 0);
        let pending: i64 = profile
            .database()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM profile_receive_pending WHERE account=?1 AND id=?2",
                    params![
                        a.public().ed25519_pub.as_slice(),
                        ids[0].as_bytes().as_slice()
                    ],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(pending, 0);
        let b_pending: i64 = profile
            .database()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM profile_receive_pending WHERE account=?1 AND id=?2",
                    params![
                        b.public().ed25519_pub.as_slice(),
                        ids[0].as_bytes().as_slice()
                    ],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(
            b_pending, 1,
            "retiring A's source must not clear B's pending key"
        );
        let work = ProfileReceiveWork::open(profile).unwrap();
        assert!(!work.processed(ids[0]).unwrap());
    }

    #[test]
    fn wrong_domain_bare_fixture_and_privacy_deletion_create_no_retirement_fact() {
        use crate::eventlog::repository::{EventDomainId, EventRepository};
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let profile = open_storage(&dir, &DeviceIdentity::generate(), &account);
        let work = ProfileReceiveWork::open(profile.clone()).unwrap();
        EventRepository::initialize_unpublished_domain(profile.database(), EventDomainId([7; 16]))
            .unwrap();
        let wrong = EventRepository::from_profile(profile.clone(), EventDomainId([7; 16])).unwrap();
        append_profiles(&wrong);
        let token = prepare_compaction(&wrong);
        assert_eq!(wrong.apply_profile_maintenance(token).unwrap().dropped, 10);
        assert_eq!(fact_count(&profile, &work.account), 0);
        let repo = EventRepository::from_profile(profile.clone(), DOMAIN).unwrap();
        append_profiles(&repo);
        assert_eq!(
            repo.drop_conversation(&ConversationId::new([1; 32]))
                .unwrap(),
            12
        );
        assert_eq!(fact_count(&profile, &work.account), 0);
        let bare_dir = tempfile::tempdir().unwrap();
        let db = Arc::new(
            crate::storage::database::EncryptedDatabase::open(
                &bare_dir.path().join("bare.db"),
                "pw",
                b"fixture",
            )
            .unwrap(),
        );
        let bare = EventRepository::new(db.clone(), DOMAIN).unwrap();
        append_profiles(&bare);
        let token = prepare_compaction(&bare);
        assert_eq!(bare.apply_profile_maintenance(token).unwrap().dropped, 10);
        let present: bool = db
            .read(|c| {
                Ok(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='profile_receive_processed')",
            [],|r|r.get(0))?)
            })
            .unwrap();
        assert!(!present);
    }
    #[test]
    fn unresolved_legacy_source_namespace_does_not_guess_the_current_account() {
        use crate::{
            eventlog::repository::EventRepository,
            storage::profile::participant::{finish_adoption, prepare_adoption},
        };
        let dir = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let a = Account::generate();
        let b = Account::generate();
        let profile = open_storage(&dir, &identity, &a);
        let repo = EventRepository::from_profile(profile.clone(), DOMAIN).unwrap();
        append_profiles(&repo);
        prepare_adoption(profile.database(), &a.public(), &b.public()).unwrap();
        finish_adoption(profile.database(), &b.public()).unwrap();
        drop(repo);
        drop(profile);
        let profile = open_storage(&dir, &identity, &b);
        profile
            .database()
            .transaction(|c| {
                c.execute(
                    "UPDATE participant_account_namespaces SET account=NULL WHERE account_id=?1",
                    [a.public().account_id()],
                )?;
                Ok(())
            })
            .unwrap();
        let repo = EventRepository::from_profile(profile.clone(), DOMAIN).unwrap();
        let token = prepare_compaction(&repo);
        assert_eq!(repo.apply_profile_maintenance(token).unwrap().dropped, 10);
        assert_eq!(fact_count(&profile, &a.public().ed25519_pub), 0);
        assert_eq!(fact_count(&profile, &b.public().ed25519_pub), 0);
    }
}
