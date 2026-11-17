//! Device-local first ingress evidence. No payload or replicated metadata lives here.
use super::*;
use crate::eventlog::repository::EventDomainId;
use crate::storage::profile::manifest;
use rusqlite::{OptionalExtension, params};

/// The authenticated immediate peer category at the first retained network ingress.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceivedVia {
    Sender,
    Member,
    PostOffice,
    LinkedDevice,
}
impl ReceivedVia {
    fn tag(self) -> i64 {
        match self {
            Self::Sender => 1,
            Self::Member => 2,
            Self::PostOffice => 3,
            Self::LinkedDevice => 4,
        }
    }
    fn from_tag(tag: i64) -> Result<Self, LogError> {
        match tag {
            1 => Ok(Self::Sender),
            2 => Ok(Self::Member),
            3 => Ok(Self::PostOffice),
            4 => Ok(Self::LinkedDevice),
            _ => Err(invalid("invalid ingress origin")),
        }
    }
}

pub(crate) struct IngressOrigin {
    pub via: ReceivedVia,
    pub peer_device: String,
}

const SCHEMA: &str = "
CREATE TABLE pending_ingress_origins(account BLOB NOT NULL CHECK(length(account)=32), domain BLOB NOT NULL CHECK(length(domain)=16), event_id BLOB NOT NULL CHECK(length(event_id)=32), conversation BLOB NOT NULL CHECK(length(conversation)=32), via INTEGER NOT NULL CHECK(via BETWEEN 1 AND 4), peer_device TEXT NOT NULL CHECK(length(peer_device)=32), PRIMARY KEY(account,domain,event_id), FOREIGN KEY(domain,event_id) REFERENCES retained_events(domain,id) ON DELETE CASCADE);
CREATE INDEX pending_ingress_source ON pending_ingress_origins(account,event_id,domain);
CREATE TABLE transcript_ingress_origins(ordinal INTEGER PRIMARY KEY REFERENCES transcript_rows(ordinal) ON DELETE CASCADE, account BLOB NOT NULL CHECK(length(account)=32), via INTEGER NOT NULL CHECK(via BETWEEN 1 AND 4), peer_device TEXT NOT NULL CHECK(length(peer_device)=32));
";

pub(super) fn initialize_on(c: &Connection) -> Result<(), LogError> {
    let present: i64 = c.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('pending_ingress_origins','pending_ingress_source','transcript_ingress_origins')", [], |r| r.get(0))?;
    if present == 0 {
        c.execute_batch(SCHEMA)?;
    } else if present != 3 {
        return Err(invalid("incomplete ingress origin schema"));
    }
    manifest::validate_ddl(c, SCHEMA)
}

impl ReceptionEffects {
    /// The caller holds the raw append transaction. Never reacquire the database.
    pub(crate) fn record_origin_on(
        &self,
        c: &Connection,
        domain: EventDomainId,
        event: &Event,
        origin: &IngressOrigin,
    ) -> Result<(), LogError> {
        self.state_on(c)?;
        if origin.peer_device.len() != 32
            || !origin.peer_device.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(invalid("invalid ingress peer device"));
        }
        c.execute("INSERT INTO pending_ingress_origins(account,domain,event_id,conversation,via,peer_device) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(account,domain,event_id) DO NOTHING", params![self.account.as_slice(),domain.0.as_slice(),event.id.as_bytes().as_slice(),event.conversation_id.as_bytes().as_slice(),origin.via.tag(),origin.peer_device])?;
        Ok(())
    }

    pub(crate) fn received_via(
        &self,
        conversation: ConversationId,
        id: EventId,
    ) -> Result<Option<ReceivedVia>, LogError> {
        self.profile.database().read(|c| {
            self.state_on(c)?;
            // Select the first source row BEFORE joining evidence. An unknown old
            // alias must not acquire a later alias's known ingress.
            let tag: Option<Option<i64>> = c.query_row("SELECT o.via FROM transcript_rows t LEFT JOIN transcript_ingress_origins o ON o.ordinal=t.ordinal AND o.account=?1 WHERE t.ordinal=(SELECT MIN(ordinal) FROM (SELECT MIN(ordinal) AS ordinal FROM transcript_rows WHERE kind=2 AND conversation=?2 AND row_key=?3 UNION ALL SELECT MIN(ordinal) FROM transcript_rows WHERE kind=3 AND conversation=?2 AND row_key=?3 UNION ALL SELECT MIN(ordinal) FROM transcript_rows WHERE kind=2 AND conversation=?2 AND logical_id=?3 UNION ALL SELECT MIN(ordinal) FROM transcript_rows WHERE kind=3 AND conversation=?2 AND logical_id=?3))", params![self.account.as_slice(),conversation.as_bytes().as_slice(),id.as_bytes().as_slice()], |r| r.get(0)).optional()?;
            tag.flatten().map(ReceivedVia::from_tag).transpose()
        })
    }

    /// File identity bundles already bound their alias count and signatures.
    /// Inspect only those exact row keys; an unknown first alias stays unknown.
    pub(crate) fn received_file_via(
        &self,
        conversation: ConversationId,
        raw: EventId,
        aliases: &[EventId],
    ) -> Result<Option<ReceivedVia>, LogError> {
        self.profile.database().read(|c| {
            self.state_on(c)?;
            file_origin_on(c, &self.account, conversation, raw, aliases)
        })
    }
}

fn file_origin_on(
    c: &Connection,
    account: &[u8; 32],
    conversation: ConversationId,
    raw: EventId,
    aliases: &[EventId],
) -> Result<Option<ReceivedVia>, LogError> {
    let mut first: Option<i64> = None;
    let mut statement = c.prepare_cached(
        "SELECT ordinal FROM transcript_rows WHERE kind=3 AND conversation=?1 AND row_key=?2",
    )?;
    for id in aliases.iter().chain(std::iter::once(&raw)) {
        let ordinal: Option<i64> = statement
            .query_row(
                params![conversation.as_bytes().as_slice(), id.as_bytes().as_slice()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(ordinal) = ordinal {
            first = Some(first.map_or(ordinal, |previous| previous.min(ordinal)));
        }
    }
    let Some(first) = first else { return Ok(None) };
    let tag: Option<i64> = c
        .query_row(
            "SELECT via FROM transcript_ingress_origins WHERE ordinal=?1 AND account=?2",
            params![first, account.as_slice()],
            |r| r.get(0),
        )
        .optional()?;
    tag.map(ReceivedVia::from_tag).transpose()
}

/// Migrate evidence only for a new reception, in the existing transcript commit.
/// Account DMs relocate the raw device conversation into their canonical account
/// conversation. Exact account + signed event ID binds provenance across that
/// relocation; the transcript still requires the exact canonical source tuple.
pub(super) fn project_on(
    c: &Connection,
    account: &[u8; 32],
    source: ReceptionSource,
) -> Result<(), LogError> {
    let ambiguous: bool = c.query_row("SELECT count(*)>1 FROM (SELECT 1 FROM pending_ingress_origins WHERE account=?1 AND event_id=?2 LIMIT 2)",params![account.as_slice(),source.event_id.as_bytes().as_slice()],|r|r.get(0))?;
    if ambiguous {
        return Err(invalid("ambiguous ingress source domain"));
    }
    c.execute("INSERT INTO transcript_ingress_origins(ordinal,account,via,peer_device) SELECT t.ordinal,p.account,p.via,p.peer_device FROM transcript_rows t JOIN pending_ingress_origins p ON p.account=?1 AND p.event_id=t.row_key WHERE t.kind=?2 AND t.conversation=?3 AND t.row_key=?4 ON CONFLICT(ordinal) DO NOTHING", params![account.as_slice(), source.kind.transcript_kind(),source.conversation.as_bytes().as_slice(),source.event_id.as_bytes().as_slice()])?;
    c.execute(
        "DELETE FROM pending_ingress_origins WHERE account=?1 AND event_id=?2",
        params![account.as_slice(), source.event_id.as_bytes().as_slice()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_origin_tags_preserve_installed_storage_contract() {
        // These literals describe already installed SQL rows, independently of
        // enum declaration order. Changing them requires a storage migration.
        for (stored, expected) in [
            (1, ReceivedVia::Sender),
            (2, ReceivedVia::Member),
            (3, ReceivedVia::PostOffice),
            (4, ReceivedVia::LinkedDevice),
        ] {
            assert_eq!(ReceivedVia::from_tag(stored).unwrap(), expected);
            assert_eq!(expected.tag(), stored);
        }
        for invalid_tag in [i64::MIN, -1, 0, 5, i64::MAX] {
            assert!(ReceivedVia::from_tag(invalid_tag).is_err());
        }
    }

    #[test]
    fn optional_schema_is_atomic_and_partial_schema_is_rejected() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE retained_events(domain BLOB,id BLOB,PRIMARY KEY(domain,id)); CREATE TABLE transcript_rows(ordinal INTEGER PRIMARY KEY);").unwrap();
        initialize_on(&c).unwrap();
        initialize_on(&c).unwrap();
        c.execute_batch("DROP TABLE transcript_ingress_origins;")
            .unwrap();
        assert!(initialize_on(&c).is_err());
    }
    #[test]
    fn optional_schema_rejects_changed_ddl_with_all_owned_names_present() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE retained_events(domain BLOB,id BLOB,PRIMARY KEY(domain,id)); CREATE TABLE transcript_rows(ordinal INTEGER PRIMARY KEY);").unwrap();
        initialize_on(&c).unwrap();
        c.execute_batch("DROP INDEX pending_ingress_source; CREATE INDEX pending_ingress_source ON pending_ingress_origins(event_id);").unwrap();
        assert!(initialize_on(&c).is_err());
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    fn database() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE retained_events(domain BLOB,id BLOB,PRIMARY KEY(domain,id)); CREATE TABLE transcript_rows(ordinal INTEGER PRIMARY KEY,kind INTEGER,conversation BLOB,row_key BLOB,logical_id BLOB);").unwrap();
        initialize_on(&c).unwrap();
        c
    }
    fn source() -> ReceptionSource {
        ReceptionSource {
            kind: ReceptionKind::DirectMessage,
            conversation: ConversationId::new([2; 32]),
            event_id: EventId::new([3; 32]),
        }
    }
    fn pending(c: &Connection) {
        c.execute(
            "INSERT INTO retained_events VALUES(?1,?2)",
            params![[1u8; 16].as_slice(), [3u8; 32].as_slice()],
        )
        .unwrap();
        c.execute(
            "INSERT INTO pending_ingress_origins VALUES(?1,?2,?3,?4,3,?5)",
            params![
                [4u8; 32].as_slice(),
                [1u8; 16].as_slice(),
                [3u8; 32].as_slice(),
                [2u8; 32].as_slice(),
                "0123456789abcdef0123456789abcdef"
            ],
        )
        .unwrap();
    }
    fn transcript(c: &Connection) {
        c.execute(
            "INSERT INTO transcript_rows VALUES(1,2,?1,?2,NULL)",
            params![[2u8; 32].as_slice(), [3u8; 32].as_slice()],
        )
        .unwrap();
    }
    fn count(c: &Connection, table: &str) -> i64 {
        c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    #[test]
    fn canonical_file_source_preserves_unknown_first_alias_and_scope() {
        let c = database();
        let host = ConversationId::new([2; 32]);
        let first = EventId::new([7; 32]);
        let later = EventId::new([8; 32]);
        for (ordinal, id) in [(1, first), (2, later)] {
            c.execute(
                "INSERT INTO transcript_rows VALUES(?1,3,?2,?3,NULL)",
                params![
                    ordinal,
                    host.as_bytes().as_slice(),
                    id.as_bytes().as_slice()
                ],
            )
            .unwrap();
        }
        c.execute(
            "INSERT INTO transcript_ingress_origins VALUES(2,?1,2,?2)",
            params![[4u8; 32].as_slice(), "0123456789abcdef0123456789abcdef"],
        )
        .unwrap();
        assert_eq!(
            file_origin_on(&c, &[4; 32], host, later, &[first, later]).unwrap(),
            None
        );
        assert_eq!(
            file_origin_on(&c, &[4; 32], host, later, &[]).unwrap(),
            Some(ReceivedVia::Member)
        );
        assert_eq!(
            file_origin_on(&c, &[5; 32], host, later, &[]).unwrap(),
            None
        );
        assert_eq!(
            file_origin_on(&c, &[4; 32], ConversationId::new([6; 32]), later, &[first]).unwrap(),
            None
        );
        c.execute("DELETE FROM transcript_rows WHERE ordinal=1", [])
            .unwrap();
        assert_eq!(
            file_origin_on(&c, &[4; 32], host, later, &[first, later]).unwrap(),
            Some(ReceivedVia::Member)
        );
    }
    #[test]
    fn origin_projection_rolls_back_with_receipt_and_cascades_with_source() {
        let c = database();
        pending(&c);
        c.execute_batch("BEGIN").unwrap();
        transcript(&c);
        project_on(&c, &[4; 32], source()).unwrap();
        assert_eq!(count(&c, "pending_ingress_origins"), 0);
        assert_eq!(count(&c, "transcript_ingress_origins"), 1);
        c.execute_batch("ROLLBACK").unwrap();
        assert_eq!(count(&c, "pending_ingress_origins"), 1);
        assert_eq!(count(&c, "transcript_ingress_origins"), 0);
        transcript(&c);
        project_on(&c, &[4; 32], source()).unwrap();
        // A duplicate installation cannot overwrite the first origin.
        c.execute(
            "INSERT INTO pending_ingress_origins VALUES(?1,?2,?3,?4,1,?5)",
            params![
                [4u8; 32].as_slice(),
                [1u8; 16].as_slice(),
                [3u8; 32].as_slice(),
                [2u8; 32].as_slice(),
                "0123456789abcdef0123456789abcdef"
            ],
        )
        .unwrap();
        project_on(&c, &[4; 32], source()).unwrap();
        assert_eq!(
            c.query_row("SELECT via FROM transcript_ingress_origins", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
        c.execute("DELETE FROM transcript_rows", []).unwrap();
        assert_eq!(count(&c, "transcript_ingress_origins"), 0);
    }
    #[test]
    fn ambiguous_domains_reject_projection_without_consuming_either_origin() {
        let c = database();
        pending(&c);
        transcript(&c);
        c.execute(
            "INSERT INTO retained_events VALUES(?1,?2)",
            params![
                [2u8; 16].as_slice(),
                source().event_id.as_bytes().as_slice()
            ],
        )
        .unwrap();
        c.execute("INSERT INTO pending_ingress_origins SELECT account,?1,event_id,conversation,1,peer_device FROM pending_ingress_origins",[[2u8;16].as_slice()]).unwrap();
        assert!(project_on(&c, &[4; 32], source()).is_err());
        assert_eq!(count(&c, "pending_ingress_origins"), 2);
        assert_eq!(count(&c, "transcript_ingress_origins"), 0);
    }

    #[test]
    fn canonical_dm_relocation_preserves_only_exact_account_and_event_origin() {
        let c = database();
        pending(&c);
        c.execute(
            "UPDATE pending_ingress_origins SET conversation=?1",
            [[9u8; 32].as_slice()],
        )
        .unwrap();
        transcript(&c);
        project_on(&c, &[5; 32], source()).unwrap();
        assert_eq!(count(&c, "transcript_ingress_origins"), 0);
        let unrelated = ReceptionSource {
            event_id: EventId::new([7; 32]),
            ..source()
        };
        project_on(&c, &[4; 32], unrelated).unwrap();
        assert_eq!(count(&c, "pending_ingress_origins"), 1);
        project_on(&c, &[4; 32], source()).unwrap();
        assert_eq!(count(&c, "pending_ingress_origins"), 0);
        assert_eq!(
            c.query_row("SELECT via FROM transcript_ingress_origins", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
    }

    #[test]
    fn absent_legacy_evidence_stays_unknown_and_other_accounts_do_not_project() {
        let c = database();
        transcript(&c);
        project_on(&c, &[4; 32], source()).unwrap();
        assert_eq!(count(&c, "transcript_ingress_origins"), 0);
        pending(&c);
        project_on(&c, &[5; 32], source()).unwrap();
        assert_eq!(count(&c, "transcript_ingress_origins"), 0);
        assert_eq!(count(&c, "pending_ingress_origins"), 1);
        c.execute("DELETE FROM retained_events", []).unwrap();
        assert_eq!(count(&c, "pending_ingress_origins"), 0);
    }
}

#[cfg(test)]
mod owner_tests {
    use super::*;
    use crate::eventlog::{EventKind, persist::PersistentEventLog};
    use crate::identity::{account::Account, device::DeviceIdentity};
    use crate::storage::profile::ProfileStorageLease;

    #[test]
    fn first_authenticated_origin_survives_reopen_and_owner_replacement_fences_writer() {
        let dir = tempfile::tempdir().unwrap();
        let device = DeviceIdentity::generate();
        let account = Account::generate();
        let open = || {
            let p = ProfileStorage::open_participant(
                ProfileStorageLease::acquire(dir.path()).unwrap(),
                &device.public(),
                &account.public(),
                &dir.path().join("messages.log"),
                &dir.path().join("sent.log"),
                "pw",
            )
            .unwrap();
            p.complete_participant("pw", None).unwrap();
            p
        };
        let profile = open();
        let mut log = PersistentEventLog::open_profile(profile.clone(), "pw").unwrap();
        let received = crate::node::received_store::ReceivedStore::from_profile(
            profile.clone(),
            &dir.path().join("received.log"),
            "pw",
        )
        .unwrap();
        let effects = ReceptionEffects::open(profile.clone()).unwrap();
        let event = Event::new(
            &device,
            ConversationId::new([8; 32]),
            1,
            vec![],
            1,
            1,
            EventKind::Message,
            vec![],
        );
        let origin = IngressOrigin {
            via: ReceivedVia::PostOffice,
            peer_device: device.public().user_id(),
        };
        // The observer has already installed its metadata when this failure
        // occurs; neither the event nor its origin may escape the transaction.
        let failed = Event::new(
            &device,
            ConversationId::new([10; 32]),
            1,
            vec![],
            1,
            1,
            EventKind::Message,
            vec![],
        );
        let result = log.append_durable_observed(failed.clone(), |c, d, e| {
            effects.record_origin_on(c, d, e, &origin)?;
            Err(invalid("injected observer failure after origin insert"))
        });
        assert!(result.is_err());
        assert!(!log.has(&failed.id).unwrap());
        let pending_count = || {
            profile
                .database()
                .read(|c| {
                    Ok(
                        c.query_row("SELECT count(*) FROM pending_ingress_origins", [], |r| {
                            r.get::<_, i64>(0)
                        })?,
                    )
                })
                .unwrap()
        };
        assert_eq!(pending_count(), 0);

        // Invalid observer input must fail the whole durable acceptance,
        // rather than leave raw data claiming a successful local observation.
        for (n, peer_device) in ["g".repeat(32), "a".repeat(31)].into_iter().enumerate() {
            let invalid_peer_event = Event::new(
                &device,
                ConversationId::new([40 + n as u8; 32]),
                1,
                vec![],
                1,
                1,
                EventKind::Message,
                vec![],
            );
            let invalid_origin = IngressOrigin {
                via: ReceivedVia::Member,
                peer_device,
            };
            let result = log.append_durable_observed(invalid_peer_event.clone(), |c, d, e| {
                effects.record_origin_on(c, d, e, &invalid_origin)
            });
            assert!(result.is_err());
            assert!(!log.has(&invalid_peer_event.id).unwrap());
            assert_eq!(pending_count(), 0);
        }

        // A later invalid batch item must roll back both earlier mutations.
        let first = Event::new(
            &device,
            ConversationId::new([11; 32]),
            1,
            vec![],
            1,
            1,
            EventKind::Message,
            vec![],
        );
        let mut invalid_second = Event::new(
            &device,
            first.conversation_id,
            2,
            vec![first.id],
            2,
            2,
            EventKind::Message,
            vec![],
        );
        invalid_second.sig[0] ^= 1;
        let mut observed = 0;
        let result = log.append_channel_batch_observed(
            &[first.clone(), invalid_second.clone()],
            |c, d, e| {
                observed += 1;
                effects.record_origin_on(c, d, e, &origin)
            },
        );
        assert!(result.is_err());
        assert_eq!(
            observed, 1,
            "the earlier valid batch item reached its observer"
        );
        assert!(!log.has(&first.id).unwrap());
        assert!(!log.has(&invalid_second.id).unwrap());
        assert_eq!(pending_count(), 0);

        // Imported/old raw events have no local origin. A later network copy
        // cannot attach an observation retroactively to that first acceptance.
        let legacy = Event::new(
            &device,
            ConversationId::new([12; 32]),
            1,
            vec![],
            1,
            1,
            EventKind::Message,
            vec![],
        );
        log.append_durable(legacy.clone()).unwrap();
        let mut duplicate_observed = false;
        log.append_durable_observed(legacy.clone(), |c, d, e| {
            duplicate_observed = true;
            effects.record_origin_on(c, d, e, &origin)
        })
        .unwrap();
        assert!(!duplicate_observed);
        assert_eq!(pending_count(), 0);
        assert_eq!(
            effects
                .received_via(legacy.conversation_id, legacy.id)
                .unwrap(),
            None
        );
        log.append_durable_observed(event.clone(), |c, d, e| {
            effects.record_origin_on(c, d, e, &origin)
        })
        .unwrap();
        let changed = IngressOrigin {
            via: ReceivedVia::Sender,
            peer_device: device.public().user_id(),
        };
        log.append_durable_observed(event.clone(), |c, d, e| {
            effects.record_origin_on(c, d, e, &changed)
        })
        .unwrap();
        let source = ReceptionSource {
            kind: ReceptionKind::DirectMessage,
            conversation: event.conversation_id,
            event_id: event.id,
        };
        profile
            .database()
            .transaction(|c| {
                super::super::super::transcript_repository::TranscriptRepository::put_on(
                    c,
                    2,
                    source.conversation,
                    source.event_id.as_bytes(),
                    1,
                    0,
                    "peer",
                    b"body",
                    false,
                    false,
                )?;
                let metadata = ReceptionMetadata {
                    from_device: device.public().user_id(),
                    received_at: 1,
                    arrival_kind: ArrivalKind::Live,
                    own_device: false,
                    batch_id: None,
                    batch_complete: true,
                    batch_size: 1,
                    reception_only: false,
                };
                install_on(c, source, &metadata)?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            effects
                .received_via(source.conversation, source.event_id)
                .unwrap(),
            Some(ReceivedVia::PostOffice)
        );
        // A logical DM can have multiple device-addressed transcript rows.
        // The earliest retained row owns the local observation even when only
        // a later alias carries known provenance.
        let logical = EventId::new([31; 32]);
        let first_alias = EventId::new([32; 32]);
        let later_alias = EventId::new([33; 32]);
        profile.database().transaction(|c| {
            for alias in [first_alias,later_alias] {
                super::super::super::transcript_repository::TranscriptRepository::put_on(c,2,source.conversation,alias.as_bytes(),1,0,"peer",b"alias",false,false)?;
                c.execute("UPDATE transcript_rows SET logical_id=?1 WHERE kind=2 AND conversation=?2 AND row_key=?3",params![logical.as_bytes().as_slice(),source.conversation.as_bytes().as_slice(),alias.as_bytes().as_slice()])?;
            }
            c.execute("INSERT INTO transcript_ingress_origins SELECT ordinal,?1,2,?2 FROM transcript_rows WHERE kind=2 AND conversation=?3 AND row_key=?4",params![account.public().ed25519_pub.as_slice(),device.public().user_id(),source.conversation.as_bytes().as_slice(),later_alias.as_bytes().as_slice()])?;
            Ok(())
        }).unwrap();
        assert_eq!(
            effects.received_via(source.conversation, logical).unwrap(),
            None
        );
        assert_eq!(
            effects
                .received_via(source.conversation, later_alias)
                .unwrap(),
            Some(ReceivedVia::Member)
        );
        assert_eq!(
            effects
                .received_via(ConversationId::new([34; 32]), logical)
                .unwrap(),
            None
        );
        profile.database().transaction(|c| {
            c.execute("DELETE FROM transcript_rows WHERE kind=2 AND conversation=?1 AND logical_id=?2",params![source.conversation.as_bytes().as_slice(),logical.as_bytes().as_slice()])?;
            Ok(())
        }).unwrap();
        drop(log);
        drop(effects);
        drop(received);
        drop(profile);
        let profile = open();
        let effects = ReceptionEffects::open(profile.clone()).unwrap();
        assert_eq!(
            effects
                .received_via(source.conversation, source.event_id)
                .unwrap(),
            Some(ReceivedVia::PostOffice)
        );
        // Erasing the materialized source reclaims its origin. Retaining and
        // receiving the same raw event again cannot recreate erased metadata.
        profile
            .database()
            .transaction(|c| {
                c.execute(
                    "DELETE FROM transcript_rows WHERE kind=2 AND conversation=?1 AND row_key=?2",
                    params![
                        source.conversation.as_bytes().as_slice(),
                        source.event_id.as_bytes().as_slice()
                    ],
                )?;
                Ok(())
            })
            .unwrap();
        let mut log = PersistentEventLog::open_profile(profile.clone(), "pw").unwrap();
        let mut erased_duplicate_observed = false;
        log.append_durable_observed(event.clone(), |c, d, e| {
            erased_duplicate_observed = true;
            effects.record_origin_on(c, d, e, &changed)
        })
        .unwrap();
        assert!(!erased_duplicate_observed);
        assert_eq!(
            effects
                .received_via(source.conversation, source.event_id)
                .unwrap(),
            None
        );
        profile
            .database()
            .read(|c| {
                assert_eq!(
                    c.query_row("SELECT count(*) FROM transcript_ingress_origins", [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
                assert_eq!(
                    c.query_row("SELECT count(*) FROM pending_ingress_origins", [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
                Ok(())
            })
            .unwrap();
        let next = Account::generate();
        crate::storage::profile::participant::prepare_adoption(
            profile.database(),
            &account.public(),
            &next.public(),
        )
        .unwrap();
        crate::storage::profile::participant::finish_adoption(profile.database(), &next.public())
            .unwrap();
        assert!(matches!(
            effects.received_via(source.conversation, source.event_id),
            Err(LogError::StaleCursor)
        ));
        let rejected = profile
            .database()
            .transaction(|c| effects.record_origin_on(c, EventDomainId([0; 16]), &event, &changed));
        assert!(matches!(rejected, Err(LogError::StaleCursor)));
    }
}
