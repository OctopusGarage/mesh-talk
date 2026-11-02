//! Current receive policy and exact offer decisions on the participant connection.
//! The single legacy file is published once; every decision remains account-local.
#[cfg(test)]
use super::ReceiveDecision;
use super::{MAGIC, Record};
use crate::{
    eventlog::{ConversationId, EventId, LogError},
    identity::account::AccountPublic,
    node::transcript_repository::{ATTACHMENT_CONSENT, publication::PublishedHistorySource},
    storage::{
        profile::{
            context::ProfileStorage,
            participant::event_ownership::{self, ParticipantEventOwner},
        },
        record_log::EncryptedRecordLog,
    },
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, sync::Arc};

const SCHEMA: &str = "
CREATE TABLE attachment_policy_state(account TEXT PRIMARY KEY REFERENCES participant_account_namespaces(account_id),global_auto INTEGER NOT NULL CHECK(global_auto IN(0,1)),generation INTEGER NOT NULL CHECK(typeof(generation)='integer' AND generation>=0),next_sequence INTEGER NOT NULL CHECK(typeof(next_sequence)='integer' AND next_sequence>=0)) WITHOUT ROWID;
CREATE TABLE attachment_contact_policy(account TEXT NOT NULL REFERENCES attachment_policy_state(account),contact TEXT NOT NULL CHECK(length(contact)=32),auto INTEGER NOT NULL CHECK(auto IN(0,1)),PRIMARY KEY(account,contact)) WITHOUT ROWID;
CREATE TABLE attachment_offer_decisions(account TEXT NOT NULL REFERENCES attachment_policy_state(account),event BLOB NOT NULL CHECK(length(event)=32),accepted INTEGER NOT NULL CHECK(accepted IN(0,1)),sequence INTEGER NOT NULL CHECK(typeof(sequence)='integer' AND sequence>0),PRIMARY KEY(account,event),UNIQUE(account,sequence)) WITHOUT ROWID;
";
fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}
fn active_on(c: &Connection) -> Result<AccountPublic, LogError> {
    let account: Vec<u8> = c.query_row(
        "SELECT account FROM participant_profile WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    Ok(AccountPublic {
        ed25519_pub: account
            .try_into()
            .map_err(|_| invalid("invalid attachment policy account"))?,
    })
}
fn initialize_account_on(c: &Connection, account: &str) -> Result<(), LogError> {
    c.execute(
        "INSERT OR IGNORE INTO attachment_policy_state VALUES(?1,0,0,0)",
        [account],
    )?;
    Ok(())
}
fn put_on(c: &Connection, account: &str, record: Record) -> Result<(), LogError> {
    if let Record::Contact { account, .. } = &record
        && (account.len() != 32 || !account.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(LogError::Serialization("invalid account id".into()));
    }
    initialize_account_on(c, account)?;
    if c.execute("UPDATE attachment_policy_state SET generation=generation+1 WHERE account=?1 AND generation<9223372036854775807",[account])?!=1 { return Err(LogError::RepositoryCounterExhausted); }
    match record {
        Record::Global(auto) => {
            c.execute(
                "UPDATE attachment_policy_state SET global_auto=?2 WHERE account=?1",
                params![account, auto],
            )?;
        }
        Record::Contact {
            account: contact,
            auto: Some(auto),
        } => {
            c.execute("INSERT INTO attachment_contact_policy VALUES(?1,?2,?3) ON CONFLICT(account,contact) DO UPDATE SET auto=excluded.auto",params![account,contact,auto])?;
        }
        Record::Contact {
            account: contact,
            auto: None,
        } => {
            c.execute(
                "DELETE FROM attachment_contact_policy WHERE account=?1 AND contact=?2",
                params![account, contact],
            )?;
        }
        Record::File { event, accept } => {
            let sequence:Option<i64>=c.query_row("UPDATE attachment_policy_state SET next_sequence=next_sequence+1 WHERE account=?1 AND next_sequence<9223372036854775807 RETURNING next_sequence",[account],|r|r.get(0)).optional()?;
            let sequence = sequence.ok_or(LogError::RepositoryCounterExhausted)?;
            c.execute("INSERT INTO attachment_offer_decisions VALUES(?1,?2,?3,?4) ON CONFLICT(account,event) DO UPDATE SET accepted=excluded.accepted,sequence=excluded.sequence",params![account,event.as_bytes().as_slice(),accept,sequence])?;
        }
    }
    Ok(())
}

pub(crate) struct IndexedAttachmentConsent {
    source: PublishedHistorySource,
    owner: Option<ParticipantEventOwner>,
}
impl IndexedAttachmentConsent {
    pub(crate) fn authenticate_legacy(
        profile: &ProfileStorage,
        path: &Path,
        password: &str,
    ) -> Result<(), LogError> {
        PublishedHistorySource::authenticate_legacy::<Record>(
            profile,
            path,
            password,
            ATTACHMENT_CONSENT,
            MAGIC,
        )
    }
    pub(crate) fn from_profile(
        profile: Arc<ProfileStorage>,
        path: &Path,
        password: &str,
    ) -> Result<Self, LogError> {
        profile.database().transaction(|c| {
            let present: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='attachment_policy_state')",
                [],
                |r| r.get(0),
            )?;
            let registry: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='auxiliary_history_sources')",
                [],
                |r| r.get(0),
            )?;
            let imported = registry
                && c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM auxiliary_history_sources WHERE kind=?1)",
                    [ATTACHMENT_CONSENT],
                    |r| r.get::<_, bool>(0),
                )?;
            if !present && !imported {
                c.execute_batch(SCHEMA)?;
            }
            crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
        })?;
        let source = PublishedHistorySource::open(
            profile,
            path,
            password,
            ATTACHMENT_CONSENT,
            |c, path, password| {
                let mut namespace = None;
                let log = EncryptedRecordLog::<Record>::open_with_visitor(
                    path,
                    password,
                    MAGIC,
                    |record| {
                        if namespace.is_none() {
                            namespace = Some(event_ownership::legacy_source_namespace_on(
                                c,
                                "attachment consent",
                            )?);
                        }
                        put_on(
                            c,
                            namespace.as_deref().expect("resolved legacy namespace"),
                            record,
                        )
                    },
                )?;
                log.sync()
            },
        )?;
        let mut store = Self {
            source,
            owner: None,
        };
        // Bootstrap migration precedes the existing adoption publication. It may
        // import old facts, but only the final recapture can expose new effects.
        store.capture(true)?;
        Ok(store)
    }
    fn capture(&mut self, migration: bool) -> Result<(), LogError> {
        self.owner = self.source.profile().database().transaction(|c| {
            let active = active_on(c)?;
            initialize_account_on(c, &active.account_id())?;
            match event_ownership::capture_event_owner_on(c, &active) {
                Ok(owner) => Ok(Some(owner)),
                Err(LogError::StaleCursor) if migration => Ok(None),
                Err(error) => Err(error),
            }
        })?;
        Ok(())
    }
    pub(crate) fn recapture(&mut self) -> Result<(), LogError> {
        self.capture(false)
    }
    fn check_on<'a>(&'a self, c: &Connection) -> Result<&'a ParticipantEventOwner, LogError> {
        let owner = self.owner.as_ref().ok_or(LogError::StaleCursor)?;
        event_ownership::check_event_owner_on(c, owner)?;
        Ok(owner)
    }
    fn write(&mut self, record: Record) -> Result<(), LogError> {
        self.source.check_write()?;
        self.source.profile().database().transaction(|c| {
            let owner = self.check_on(c)?;
            put_on(c, &owner.namespace(), record)
        })
    }
    pub(crate) fn generation(&self) -> Result<u64, LogError> {
        self.source.profile().database().read(|c| {
            let owner = self.check_on(c)?;
            let value: i64 = c.query_row(
                "SELECT generation FROM attachment_policy_state WHERE account=?1",
                [owner.namespace()],
                |r| r.get(0),
            )?;
            u64::try_from(value).map_err(|_| invalid("invalid attachment policy generation"))
        })
    }
    pub(crate) fn global_auto(&self) -> Result<bool, LogError> {
        self.source.profile().database().read(|c| {
            let owner = self.check_on(c)?;
            Ok(c.query_row(
                "SELECT global_auto FROM attachment_policy_state WHERE account=?1",
                [owner.namespace()],
                |r| r.get(0),
            )?)
        })
    }
    pub(crate) fn contact_auto(&self, contact: &str) -> Result<Option<bool>, LogError> {
        self.source.profile().database().read(|c| {
            let owner = self.check_on(c)?;
            Ok(c.query_row(
                "SELECT auto FROM attachment_contact_policy WHERE account=?1 AND contact=?2",
                params![owner.namespace(), contact],
                |r| r.get(0),
            )
            .optional()?)
        })
    }
    pub(crate) fn preference(&self, contact: &str) -> Result<bool, LogError> {
        self.source.profile().database().read(|c|{
            let owner=self.check_on(c)?;
            Ok(c.query_row("SELECT COALESCE((SELECT auto FROM attachment_contact_policy WHERE account=?1 AND contact=?2),global_auto) FROM attachment_policy_state WHERE account=?1",params![owner.namespace(),contact],|r|r.get(0))?)
        })
    }
    #[cfg(test)]
    pub(crate) fn decision(&self, event: EventId) -> Result<Option<(bool, i64)>, LogError> {
        self.source.profile().database().read(|c|{
            let owner=self.check_on(c)?;
            Ok(c.query_row("SELECT accepted,sequence FROM attachment_offer_decisions WHERE account=?1 AND event=?2",params![owner.namespace(),event.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?)
        })
    }
    /// MAX is a scalar aggregate over exact indexed joins; it never constructs
    /// an alias list or a sort proportional to retained file history.
    pub(crate) fn file_decision(&self, file: ConversationId) -> Result<Option<bool>, LogError> {
        self.source.profile().database().read(|c|{
            let owner=self.check_on(c)?;
            Ok(c.query_row("SELECT accepted FROM attachment_offer_decisions WHERE account=?1 AND sequence=(SELECT MAX(d.sequence) FROM file_book_offers o JOIN attachment_offer_decisions d ON d.account=?1 AND d.event=o.event WHERE o.account=?2 AND o.file=?3)",params![owner.namespace(),owner.account().as_slice(),file.as_bytes().as_slice()],|r|r.get(0)).optional()?)
        })
    }
    pub(crate) fn set_global_auto(&mut self, auto: bool) -> Result<(), LogError> {
        self.write(Record::Global(auto))
    }
    pub(crate) fn set_contact_auto(
        &mut self,
        account: &str,
        auto: Option<bool>,
    ) -> Result<(), LogError> {
        self.write(Record::Contact {
            account: account.into(),
            auto,
        })
    }
    pub(crate) fn decide(&mut self, event: EventId, accept: bool) -> Result<(), LogError> {
        self.write(Record::File { event, accept })
    }
    #[cfg(test)]
    pub(crate) fn status(
        &self,
        event: EventId,
        contact: &str,
    ) -> Result<ReceiveDecision, LogError> {
        Ok(match self.decision(event)?.map(|(accepted, _)| accepted) {
            Some(true) => ReceiveDecision::Accepted,
            Some(false) => ReceiveDecision::Declined,
            None if self.preference(contact)? => ReceiveDecision::Accepted,
            None => ReceiveDecision::Pending,
        })
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::{
        identity::{account::Account, device::DeviceIdentity},
        storage::profile::{ProfileStorageLease, participant},
    };
    pub(in crate::node::attachment_consent) fn profile(
        root: &Path,
        device: &DeviceIdentity,
        account: &Account,
    ) -> Arc<ProfileStorage> {
        let profile = ProfileStorage::open_participant(
            ProfileStorageLease::acquire(root).unwrap(),
            &device.public(),
            &account.public(),
            &root.join("messages.log"),
            &root.join("sent.log"),
            "pw",
        )
        .unwrap();
        profile.complete_participant("pw", None).unwrap();
        drop(
            crate::eventlog::persist::PersistentEventLog::open_profile(profile.clone(), "pw")
                .unwrap(),
        );
        profile
    }
    #[test]
    fn indexed_attachment_policy_preserves_accounts_and_stale_generations() {
        let root = tempfile::tempdir().unwrap();
        let device = DeviceIdentity::generate();
        let a = Account::generate();
        let b = Account::generate();
        let profile = profile(root.path(), &device, &a);
        let path = root.path().join("consent.log");
        let event = EventId::new([1; 32]);
        let contact = "a".repeat(32);
        let mut old = IndexedAttachmentConsent::from_profile(profile.clone(), &path, "pw").unwrap();
        old.set_global_auto(true).unwrap();
        old.set_contact_auto(&contact, Some(false)).unwrap();
        old.decide(event, false).unwrap();
        let generation = old.generation().unwrap();
        participant::prepare_adoption(profile.database(), &a.public(), &b.public()).unwrap();
        participant::finish_adoption(profile.database(), &b.public()).unwrap();
        let mut other =
            IndexedAttachmentConsent::from_profile(profile.clone(), &path, "pw").unwrap();
        assert!(!other.global_auto().unwrap());
        assert_eq!(other.contact_auto(&contact).unwrap(), None);
        assert_eq!(
            other.status(event, &contact).unwrap(),
            ReceiveDecision::Pending
        );
        other.decide(event, true).unwrap();
        assert!(matches!(
            old.decide(event, true),
            Err(LogError::StaleCursor)
        ));
        participant::prepare_adoption(profile.database(), &b.public(), &a.public()).unwrap();
        participant::finish_adoption(profile.database(), &a.public()).unwrap();
        let resumed = IndexedAttachmentConsent::from_profile(profile, &path, "pw").unwrap();
        assert_eq!(resumed.generation().unwrap(), generation);
        assert!(resumed.global_auto().unwrap());
        assert_eq!(resumed.contact_auto(&contact).unwrap(), Some(false));
        assert_eq!(
            resumed.status(event, &contact).unwrap(),
            ReceiveDecision::Declined
        );
        assert!(matches!(old.global_auto(), Err(LogError::StaleCursor)));
        assert!(matches!(
            other.decide(event, false),
            Err(LogError::StaleCursor)
        ));
    }
    #[test]
    fn indexed_attachment_policy_failed_write_keeps_decision_and_generation() {
        let root = tempfile::tempdir().unwrap();
        let device = DeviceIdentity::generate();
        let account = Account::generate();
        let profile = profile(root.path(), &device, &account);
        let path = root.path().join("consent.log");
        let mut consent =
            IndexedAttachmentConsent::from_profile(profile.clone(), &path, "pw").unwrap();
        let event = EventId::new([2; 32]);
        consent.decide(event, false).unwrap();
        let generation = consent.generation().unwrap();
        profile.database().transaction(|c|{c.execute_batch("CREATE TRIGGER fail_attachment_decision BEFORE UPDATE ON attachment_offer_decisions BEGIN SELECT RAISE(ABORT,'injected consent failure'); END")?;Ok(())}).unwrap();
        assert!(consent.decide(event, true).is_err());
        assert_eq!(consent.generation().unwrap(), generation);
        assert_eq!(
            consent.decision(event).unwrap().map(|(value, _)| value),
            Some(false)
        );
        profile
            .database()
            .transaction(|c| {
                c.execute_batch("DROP TRIGGER fail_attachment_decision")?;
                Ok(())
            })
            .unwrap();
        consent.decide(event, true).unwrap();
        assert_eq!(consent.generation().unwrap(), generation + 1);
        drop(consent);
        let reopened = IndexedAttachmentConsent::from_profile(profile, &path, "pw").unwrap();
        assert_eq!(
            reopened.status(event, &"a".repeat(32)).unwrap(),
            ReceiveDecision::Accepted
        );
    }
    #[test]
    fn indexed_attachment_import_rejects_ambiguous_accounts_and_preserves_source() {
        let root = tempfile::tempdir().unwrap();
        let device = DeviceIdentity::generate();
        let a = Account::generate();
        let b = Account::generate();
        let profile = profile(root.path(), &device, &a);
        let path = root.path().join("consent.log");
        let mut legacy = super::super::AttachmentConsent::open(&path, "pw").unwrap();
        legacy.set_global_auto(true).unwrap();
        drop(legacy);
        let original = std::fs::read(&path).unwrap();
        participant::prepare_adoption(profile.database(), &a.public(), &b.public()).unwrap();
        participant::finish_adoption(profile.database(), &b.public()).unwrap();
        assert!(
            matches!(IndexedAttachmentConsent::from_profile(profile.clone(),&path,"pw"),Err(LogError::CorruptFile(error)) if error.contains("ownership is unavailable"))
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let count: i64 = profile
            .database()
            .read(|c| {
                Ok(
                    c.query_row("SELECT count(*) FROM attachment_offer_decisions", [], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(count, 0);
        // Retrying must preserve the ownership error, never turn it into empty state.
        let empty =
            IndexedAttachmentConsent::from_profile(profile, &root.path().join("consent.log"), "pw");
        assert!(
            empty.is_err(),
            "an existing ambiguous source must not become an empty successful policy"
        );
    }
    #[test]
    fn indexed_attachment_empty_legacy_source_needs_no_guessed_owner() {
        let root = tempfile::tempdir().unwrap();
        let device = DeviceIdentity::generate();
        let first = Account::generate();
        let second = Account::generate();
        let profile = profile(root.path(), &device, &first);
        participant::prepare_adoption(profile.database(), &first.public(), &second.public())
            .unwrap();
        participant::finish_adoption(profile.database(), &second.public()).unwrap();
        let policy =
            IndexedAttachmentConsent::from_profile(profile, &root.path().join("consent.log"), "pw")
                .unwrap();
        assert!(!policy.global_auto().unwrap());
        assert_eq!(policy.generation().unwrap(), 0);
        assert_eq!(policy.decision(EventId::new([9; 32])).unwrap(), None);
    }

    #[test]
    fn indexed_attachment_import_keeps_authenticated_previous_owner() {
        let root = tempfile::tempdir().unwrap();
        let device = DeviceIdentity::generate();
        let previous = Account::generate();
        let current = Account::generate();
        let path = root.path().join("consent.log");
        let event = EventId::new([3; 32]);
        let mut legacy = super::super::AttachmentConsent::open(&path, "pw").unwrap();
        legacy.set_global_auto(true).unwrap();
        legacy.decide(event, false).unwrap();
        drop(legacy);
        let profile = ProfileStorage::open_participant(
            ProfileStorageLease::acquire(root.path()).unwrap(),
            &device.public(),
            &current.public(),
            &root.path().join("messages.log"),
            &root.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        profile
            .complete_participant("pw", Some(&previous.account_id()))
            .unwrap();
        let mut consent =
            IndexedAttachmentConsent::from_profile(profile.clone(), &path, "pw").unwrap();
        assert!(matches!(consent.global_auto(), Err(LogError::StaleCursor)));
        drop(
            crate::eventlog::persist::PersistentEventLog::open_profile(profile.clone(), "pw")
                .unwrap(),
        );
        participant::finish_adoption(profile.database(), &current.public()).unwrap();
        consent.recapture().unwrap();
        assert!(!consent.global_auto().unwrap());
        assert_eq!(consent.decision(event).unwrap(), None);
        participant::prepare_adoption(profile.database(), &current.public(), &previous.public())
            .unwrap();
        participant::finish_adoption(profile.database(), &previous.public()).unwrap();
        let old = IndexedAttachmentConsent::from_profile(profile, &path, "pw").unwrap();
        assert!(old.global_auto().unwrap());
        assert_eq!(
            old.status(event, &"a".repeat(32)).unwrap(),
            ReceiveDecision::Declined
        );
    }
}
