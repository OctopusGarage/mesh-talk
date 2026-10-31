//! Exact logical-file bindings on the participant connection; retained bundles
//! and alias history never become a resident map.
mod acceptance;

use super::*;
use crate::node::transcript_repository::{
    FILE_IDENTITIES, FILES, publication::PublishedHistorySource,
};
use crate::storage::profile::{context::ProfileStorage, manifest};
use rusqlite::{Connection, params};
use std::sync::Arc;

const SCHEMA: &str = "
CREATE TABLE file_identity_usage(singleton INTEGER PRIMARY KEY CHECK(singleton=1), identities INTEGER NOT NULL CHECK(identities>=0), aliases INTEGER NOT NULL CHECK(aliases>=0));
CREATE TABLE file_identity_headers(host BLOB NOT NULL CHECK(length(host)=32), logical BLOB NOT NULL CHECK(length(logical)=32), id BLOB NOT NULL UNIQUE CHECK(length(id)=32), author BLOB NOT NULL CHECK(length(author)=32), file BLOB NOT NULL CHECK(length(file)=32), manifest_digest BLOB NOT NULL CHECK(length(manifest_digest)=32), lamport BLOB NOT NULL CHECK(length(lamport)=8), clock BLOB NOT NULL CHECK(length(clock)=8), ciphertext_bytes INTEGER NOT NULL CHECK(ciphertext_bytes>0), PRIMARY KEY(host,logical)) WITHOUT ROWID;
CREATE TABLE file_identity_bodies(host BLOB NOT NULL, logical BLOB NOT NULL, ciphertext BLOB NOT NULL, signature BLOB NOT NULL CHECK(length(signature)=64), PRIMARY KEY(host,logical), FOREIGN KEY(host,logical) REFERENCES file_identity_headers(host,logical) ON DELETE CASCADE) WITHOUT ROWID;
CREATE TABLE file_identity_aliases(manifest BLOB PRIMARY KEY CHECK(length(manifest)=32), host BLOB NOT NULL CHECK(length(host)=32), logical BLOB NOT NULL CHECK(length(logical)=32), FOREIGN KEY(host,logical) REFERENCES file_identity_headers(host,logical) ON DELETE CASCADE) WITHOUT ROWID;
CREATE INDEX file_identity_alias_bindings ON file_identity_aliases(host,logical,manifest);
";
fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}
fn blob<const N: usize>(row: &rusqlite::Row<'_>, index: usize) -> Result<[u8; N], LogError> {
    row.get_ref(index)?
        .as_blob()
        .map_err(|_| invalid("invalid file identity metadata"))?
        .try_into()
        .map_err(|_| invalid("invalid file identity width"))
}
pub(crate) struct IndexedFileMessageIdentities {
    source: PublishedHistorySource,
}
impl IndexedFileMessageIdentities {
    pub(crate) fn authenticate(
        profile: &ProfileStorage,
        path: &Path,
        password: &str,
    ) -> Result<(), LogError> {
        PublishedHistorySource::authenticate_legacy_with::<Event>(
            profile,
            path,
            password,
            FILE_IDENTITIES,
            MAGIC,
            |event| {
                decode(&event)
                    .map(|_| ())
                    .ok_or_else(|| invalid("invalid file identity source"))
            },
        )
    }
    /// Bindings are append-only; the persisted accepted-binding count is also
    /// the exact revision used by ordered views and archive snapshots.
    pub(crate) fn revision(&self) -> Result<u64, LogError> {
        self.source.profile().database().read(|c| {
            let revision: i64 = c.query_row(
                "SELECT identities FROM file_identity_usage WHERE singleton=1",
                [],
                |row| row.get(0),
            )?;
            u64::try_from(revision).map_err(|_| invalid("negative file identity revision"))
        })
    }
    pub(crate) fn from_profile(
        profile: Arc<ProfileStorage>,
        path: &Path,
        password: &str,
    ) -> Result<Self, LogError> {
        profile
            .participant_database()
            .ok_or_else(|| invalid("file identity profile is not initialized"))?
            .transaction(|c| {
                let registry: bool = c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='transcript_sources')",
                    [],
                    |r| r.get(0),
                )?;
                let imported = registry
                    && c.query_row(
                        "SELECT EXISTS(SELECT 1 FROM transcript_sources WHERE kind=?1)",
                        [FILE_IDENTITIES],
                        |r| r.get::<_, bool>(0),
                    )?;
                let present: bool = c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='file_identity_headers')",
                    [],
                    |r| r.get(0),
                )?;
                if !present && !imported {
                    c.execute_batch(SCHEMA)?;
                    c.execute("INSERT INTO file_identity_usage VALUES(1,0,0)", [])?;
                }
                manifest::validate_ddl(c, SCHEMA)?;
                acceptance::initialize_on(c)
            })?;
        let source = PublishedHistorySource::open(
            profile,
            path,
            password,
            FILE_IDENTITIES,
            |c, path, password| {
                let writer = EncryptedRecordLog::<Event>::open_with_visitor(
                    path,
                    password,
                    MAGIC,
                    |event| Self::put_on(c, &event).map(|_| ()),
                )?;
                writer.sync()
            },
        )?;
        Ok(Self { source })
    }
    fn binding_on(
        c: &Connection,
        manifest: EventId,
    ) -> Result<Option<(ConversationId, EventId)>, LogError> {
        let mut statement =
            c.prepare_cached("SELECT host,logical FROM file_identity_aliases WHERE manifest=?1")?;
        let mut rows = statement.query([manifest.as_bytes().as_slice()])?;
        rows.next()?
            .map(|row| {
                Ok((
                    ConversationId::new(blob(row, 0)?),
                    EventId::new(blob(row, 1)?),
                ))
            })
            .transpose()
    }
    fn load_on(
        c: &Connection,
        host: ConversationId,
        logical: EventId,
    ) -> Result<Option<(Event, Identity)>, LogError> {
        let mut statement=c.prepare_cached("SELECT id,author,file,manifest_digest,lamport,clock,ciphertext_bytes FROM file_identity_headers WHERE host=?1 AND logical=?2")?;
        let mut rows = statement.query(params![
            host.as_bytes().as_slice(),
            logical.as_bytes().as_slice()
        ])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let id = EventId::new(blob(row, 0)?);
        let author = Author::from_ed25519(blob(row, 1)?);
        let file = ConversationId::new(blob(row, 2)?);
        let manifest_digest: [u8; 32] = blob(row, 3)?;
        let lamport = u64::from_be_bytes(blob(row, 4)?);
        let wall_clock = u64::from_be_bytes(blob(row, 5)?);
        let bytes: usize = usize::try_from(row.get::<_, i64>(6)?)
            .map_err(|_| invalid("invalid identity allocation"))?;
        if bytes > PREFIX.len() + 8192 {
            return Err(invalid("file identity exceeds payload budget"));
        }
        let lengths:(i64,i64)=c.prepare_cached("SELECT length(ciphertext),length(signature) FROM file_identity_bodies WHERE host=?1 AND logical=?2")?.query_row(params![host.as_bytes().as_slice(),logical.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if lengths != (bytes as i64, 64) {
            return Err(invalid("file identity body disagrees with metadata"));
        }
        let (ciphertext,sig)=c.prepare_cached("SELECT ciphertext,signature FROM file_identity_bodies WHERE host=?1 AND logical=?2")?.query_row(params![host.as_bytes().as_slice(),logical.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let event = Event {
            id,
            conversation_id: scope(logical),
            author,
            seq: 1,
            parents: vec![],
            lamport,
            wall_clock,
            kind: EventKind::Message,
            ciphertext,
            sig,
        };
        let identity = decode(&event).ok_or_else(|| invalid("invalid persisted file identity"))?;
        if identity.host() != host
            || identity.logical != logical
            || identity.file != file
            || identity.manifest_digest != manifest_digest
        {
            return Err(invalid("file identity header disagrees with signed body"));
        }
        Ok(Some((event, identity)))
    }
    fn put_on(c: &Connection, event: &Event) -> Result<bool, LogError> {
        let identity = decode(event)
            .ok_or_else(|| LogError::AdmissionRejected("invalid logical file identity".into()))?;
        let host = identity.host();
        let logical = identity.logical;
        if let Some((old, _)) = Self::load_on(c, host, logical)? {
            return if old == *event {
                Ok(false)
            } else {
                Err(LogError::AdmissionRejected(
                    "conflicting logical file identity".into(),
                ))
            };
        }
        let (count, aliases): (i64, i64) = c.query_row(
            "SELECT identities,aliases FROM file_identity_usage WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if count < 0 || aliases < 0 {
            return Err(invalid("invalid file identity count"));
        }
        if count >= MAX_IDENTITIES as i64
            || aliases + identity.aliases.len() as i64 > MAX_INDEXED_ALIASES as i64
        {
            return Err(std::io::Error::other("logical file identity capacity exhausted").into());
        }
        for alias in &identity.aliases {
            if Self::binding_on(c, *alias)?.is_some_and(|key| key != (host, logical)) {
                return Err(LogError::AdmissionRejected(
                    "conflicting logical file identity scope".into(),
                ));
            }
        }
        c.execute(
            "INSERT INTO file_identity_headers VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                host.as_bytes().as_slice(),
                logical.as_bytes().as_slice(),
                event.id.as_bytes().as_slice(),
                event.author.ed25519_pub().as_slice(),
                identity.file.as_bytes().as_slice(),
                identity.manifest_digest.as_slice(),
                event.lamport.to_be_bytes().as_slice(),
                event.wall_clock.to_be_bytes().as_slice(),
                event.ciphertext.len() as i64
            ],
        )?;
        c.execute(
            "INSERT INTO file_identity_bodies VALUES(?1,?2,?3,?4)",
            params![
                host.as_bytes().as_slice(),
                logical.as_bytes().as_slice(),
                &event.ciphertext,
                &event.sig
            ],
        )?;
        for alias in &identity.aliases {
            c.execute(
                "INSERT INTO file_identity_aliases VALUES(?1,?2,?3)",
                params![
                    alias.as_bytes().as_slice(),
                    host.as_bytes().as_slice(),
                    logical.as_bytes().as_slice()
                ],
            )?;
        }
        acceptance::insert_on(c, host, logical, count + 1)?;
        c.execute("UPDATE file_identity_usage SET identities=identities+1,aliases=aliases+?1 WHERE singleton=1",[identity.aliases.len() as i64])?;
        Ok(true)
    }
    pub(super) fn insert(&mut self, event: Event, identity: Identity) -> Result<bool, LogError> {
        if decode(&event).as_ref() != Some(&identity) {
            return Err(LogError::AdmissionRejected(
                "file identity input disagrees with signature".into(),
            ));
        }
        self.source.check_write()?;
        self.source
            .profile()
            .database()
            .transaction(|c| Self::put_on(c, &event))
    }
    pub(crate) fn contains(&self, host: ConversationId, event: EventId) -> Result<bool, LogError> {
        self.source
            .profile()
            .database()
            .read(|c| Ok(Self::binding_on(c, event)?.is_some_and(|key| key.0 == host)))
    }
    pub(crate) fn logical(
        &self,
        host: ConversationId,
        event: EventId,
    ) -> Result<EventId, LogError> {
        self.source.profile().database().read(|c| {
            Ok(Self::binding_on(c, event)?
                .filter(|key| key.0 == host)
                .map_or(event, |key| key.1))
        })
    }
    fn identity_for(&self, event: EventId) -> Result<Option<(Event, Identity)>, LogError> {
        self.source
            .profile()
            .database()
            .read(|c| match Self::binding_on(c, event)? {
                Some((host, logical)) => Self::load_on(c, host, logical),
                None => Ok(None),
            })
    }
    pub(crate) fn row_logical(&self, row: &ReceivedEntry) -> Result<EventId, LogError> {
        Ok(self
            .identity_for(row.event_id)?
            .filter(|(_, identity)| identity.matches(row))
            .map_or(row.event_id, |(_, identity)| identity.logical))
    }
    pub(in crate::node) fn row_suppressed(
        &self,
        row: &ReceivedEntry,
        _suppression: &crate::node::account_suppression::IndexedAccountSuppression,
    ) -> Result<bool, LogError> {
        self.source
            .profile()
            .database()
            .read(|c| Self::row_suppressed_on(c, row))
    }
    /// Exact metadata selector shared by the FILES transaction. Identity headers
    /// have already passed the signed binding owner's admission checks.
    pub(crate) fn row_suppressed_on(c: &Connection, row: &ReceivedEntry) -> Result<bool, LogError> {
        let direct: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM transcript_suppression WHERE conversation=?1 AND id=?2)",
            params![
                row.conversation.as_bytes().as_slice(),
                row.event_id.as_bytes().as_slice()
            ],
            |r| r.get(0),
        )?;
        if direct {
            return Ok(true);
        }
        let mut statement = c.prepare_cached("SELECT h.author FROM file_identity_aliases a JOIN file_identity_headers h ON h.host=a.host AND h.logical=a.logical WHERE a.manifest=?1 AND h.manifest_digest=?2 AND EXISTS(SELECT 1 FROM transcript_suppression s WHERE s.conversation=h.host AND (s.id=h.logical OR s.id=a.manifest))")?;
        let mut rows = statement.query(params![
            row.event_id.as_bytes().as_slice(),
            digest(&row.plaintext).as_slice()
        ])?;
        Ok(rows
            .next()?
            .map(|r| blob(r, 0).map(|author| Author::from_ed25519(author).user_id() == row.from))
            .transpose()?
            .unwrap_or(false))
    }
    pub(crate) fn row_author_account(
        &self,
        row: &ReceivedEntry,
    ) -> Result<Option<String>, LogError> {
        Ok(self
            .identity_for(row.event_id)?
            .filter(|(_, identity)| identity.matches(row))
            .map(|(_, identity)| identity.source.account_id()))
    }
    pub(crate) fn aliases(
        &self,
        host: ConversationId,
        event: EventId,
    ) -> Result<Vec<EventId>, LogError> {
        Ok(self
            .identity_for(event)?
            .filter(|(_, identity)| identity.host() == host)
            .map_or_else(Vec::new, |(_, identity)| identity.aliases))
    }
    pub(crate) fn bundle(&self, event: EventId) -> Result<Option<Event>, LogError> {
        Ok(self.identity_for(event)?.map(|(event, _)| event))
    }
    /// The indexed binding was signature-validated before its atomic insertion.
    /// Untrusted file selectors alone never establish alias authority.
    pub(crate) fn logical_for_ordinal(&self, ordinal: i64) -> Result<Option<EventId>, LogError> {
        self.source.profile().database().read(|c|{
            let mut statement=c.prepare_cached("SELECT r.row_key,r.sender,h.author,h.logical FROM transcript_rows r LEFT JOIN file_identity_aliases a ON a.manifest=r.row_key AND a.host=r.conversation LEFT JOIN file_identity_headers h ON h.host=a.host AND h.logical=a.logical AND h.manifest_digest=r.plaintext_sha256 AND h.file=r.file_conversation WHERE r.ordinal=?1 AND r.kind=?2")?;
            let mut rows=statement.query(params![ordinal,FILES])?;let Some(row)=rows.next()? else{return Ok(None)};
            let original=EventId::new(blob(row,0)?);
            if matches!(row.get_ref(2)?,rusqlite::types::ValueRef::Null){return Ok(Some(original));}
            let source=Author::from_ed25519(blob(row,2)?).user_id();
            if row.get_ref(1)?.as_str().map_err(|_|invalid("invalid file row author"))?!=source {return Ok(Some(original));}
            Ok(Some(EventId::new(blob(row,3)?)))
        })
    }
    pub(crate) fn page(
        &self,
        after: Option<(ConversationId, EventId)>,
        limit: usize,
    ) -> Result<Vec<Event>, LogError> {
        if limit == 0 || limit > 128 {
            return Err(LogError::InvalidPageBudget);
        }
        self.source.profile().database().read(|c|{
            let mut statement=c.prepare_cached("SELECT host,logical FROM file_identity_headers WHERE (host,logical)>(?1,?2) ORDER BY host,logical LIMIT ?3")?;
            let mut rows=statement.query(params![after.as_ref().map(|key|key.0.as_bytes().as_slice()).unwrap_or(&[]),after.as_ref().map(|key|key.1.as_bytes().as_slice()).unwrap_or(&[]),limit as i64])?;
            let mut keys=Vec::with_capacity(limit);while let Some(row)=rows.next()? {keys.push((ConversationId::new(blob(row,0)?),EventId::new(blob(row,1)?)));}
            let mut result=Vec::with_capacity(keys.len());for (host,logical) in keys {result.push(Self::load_on(c,host,logical)?.ok_or_else(||invalid("selected file identity disappeared"))?.0);}
            Ok(result)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{account::Account, device::DeviceIdentity};
    use crate::storage::profile::ProfileStorageLease;
    fn owner(root: &Path, signer: &DeviceIdentity, account: &Account) -> Arc<ProfileStorage> {
        let storage = ProfileStorage::open_participant(
            ProfileStorageLease::acquire(root).unwrap(),
            &signer.public(),
            &account.public(),
            &root.join("events.log"),
            &root.join("sent.log"),
            "pw",
        )
        .unwrap();
        storage.complete_participant("pw", None).unwrap();
        storage
    }
    fn fixture(
        signer: &DeviceIdentity,
        account: &Account,
        target: &Account,
        n: u8,
    ) -> (Event, Identity, ReceivedEntry) {
        let file = ConversationId::new([n; 32]);
        let logical = EventId::new([n.wrapping_add(60); 32]);
        let manifest = crate::file::FileManifest {
            name: "file.txt".into(),
            size: 3,
            mime: "text/plain".into(),
            checksum: [5; 32],
            file_key: [6; 32],
            file_conv: file,
            chunk_count: 1,
        };
        let plaintext = manifest.encode();
        let identity = Identity {
            source: account.certify(&signer.public().ed25519_pub),
            recipient_account: target.account_id(),
            file,
            logical,
            manifest_digest: digest(&plaintext),
            aliases: vec![logical, EventId::new([n.wrapping_add(120); 32])],
        };
        let mut body = PREFIX.to_vec();
        body.extend(bincode::serialize(&identity).unwrap());
        let bundle = Event::new(
            signer,
            scope(logical),
            1,
            vec![],
            1,
            42,
            EventKind::Message,
            body,
        );
        let row = ReceivedEntry {
            event_id: identity.aliases[1],
            conversation: identity.host(),
            from: signer.user_id(),
            wall_clock: 42,
            plaintext,
        };
        (bundle, identity, row)
    }
    #[test]
    fn file_identity_warm_open_and_metadata_lookup_do_not_load_history_bodies() {
        let root = tempfile::tempdir().unwrap();
        let signer = DeviceIdentity::generate();
        let account = Account::generate();
        let target = Account::generate();
        let path = root.path().join("file-message-identities.log");
        let (mut legacy, _) = EncryptedRecordLog::<Event>::open(&path, "pw", MAGIC).unwrap();
        let mut last = None;
        for n in 1..=24 {
            let value = fixture(&signer, &account, &target, n);
            legacy.append_durable(&value.0).unwrap();
            last = Some(value);
        }
        drop(legacy);
        let (bundle, identity, row) = last.unwrap();
        let profile = owner(root.path(), &signer, &account);
        let store =
            IndexedFileMessageIdentities::from_profile(profile.clone(), &path, "pw").unwrap();
        assert_eq!(store.revision().unwrap(), 24);
        assert_eq!(store.bundle(row.event_id).unwrap(), Some(bundle));
        assert_eq!(store.row_logical(&row).unwrap(), identity.logical);
        let mut received = crate::node::received_store::ReceivedStore::file_cards_from_profile(
            profile.clone(),
            &root.path().join("received_files.log"),
            "pw",
        )
        .unwrap();
        received.record_file_alias_durable(&row).unwrap();
        let header = received
            .header_scan_page(None, None, Default::default())
            .unwrap()
            .rows
            .remove(0);
        // Body corruption demonstrates that this selector reads only authenticated
        // indexed bindings and exact transcript metadata, never the payload table.
        profile
            .database()
            .transaction(|c| {
                c.execute("UPDATE transcript_bodies SET plaintext=zeroblob(1)", [])?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            store.logical_for_ordinal(header.ordinal).unwrap(),
            Some(identity.logical)
        );
        profile
            .database()
            .transaction(|c| {
                c.execute("UPDATE transcript_rows SET sender='another-device'", [])?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            store.logical_for_ordinal(header.ordinal).unwrap(),
            Some(row.event_id)
        );
        drop(received);
        drop(store);
        drop(profile);
        let profile = owner(root.path(), &signer, &account);
        crate::storage::record_log::frame_observations::reset();
        let reopened = IndexedFileMessageIdentities::from_profile(profile, &path, "pw").unwrap();
        assert_eq!(
            crate::storage::record_log::frame_observations::maximum_capacity(),
            0
        );
        assert_eq!(reopened.revision().unwrap(), 24);
        assert_eq!(
            reopened.aliases(row.conversation, row.event_id).unwrap(),
            identity.aliases
        );
    }
    #[test]
    fn indexed_file_identity_rejects_cross_scope_conflicts_and_keeps_prior_binding() {
        let root = tempfile::tempdir().unwrap();
        let signer = DeviceIdentity::generate();
        let account = Account::generate();
        let target = Account::generate();
        let profile = owner(root.path(), &signer, &account);
        let mut store = IndexedFileMessageIdentities::from_profile(
            profile,
            &root.path().join("file-message-identities.log"),
            "pw",
        )
        .unwrap();
        let (bundle, identity, row) = fixture(&signer, &account, &target, 1);
        assert!(store.insert(bundle.clone(), identity.clone()).unwrap());
        assert_eq!(store.revision().unwrap(), 1);
        assert!(!store.insert(bundle, identity.clone()).unwrap());
        assert_eq!(store.revision().unwrap(), 1);
        let (_, mut conflict, _) = fixture(&signer, &account, &Account::generate(), 2);
        conflict.aliases.push(row.event_id);
        let mut body = PREFIX.to_vec();
        body.extend(bincode::serialize(&conflict).unwrap());
        let event = Event::new(
            &signer,
            scope(conflict.logical),
            1,
            vec![],
            1,
            42,
            EventKind::Message,
            body,
        );
        assert!(
            store
                .insert(event, conflict)
                .unwrap_err()
                .is_peer_rejection()
        );
        assert_eq!(store.row_logical(&row).unwrap(), identity.logical);
        assert_eq!(store.page(None, 128).unwrap().len(), 1);
        assert_eq!(store.revision().unwrap(), 1);
    }
    #[test]
    fn indexed_file_identity_acceptance_is_atomic_bounded_and_body_free() {
        let root = tempfile::tempdir().unwrap();
        let signer = DeviceIdentity::generate();
        let account = Account::generate();
        let target = Account::generate();
        let profile = owner(root.path(), &signer, &account);
        let path = root.path().join("file-message-identities.log");
        let mut store =
            IndexedFileMessageIdentities::from_profile(profile.clone(), &path, "pw").unwrap();
        let host = fixture(&signer, &account, &target, 1).1.host();
        for n in 1..=24 {
            let (bundle, identity, _) = fixture(&signer, &account, &target, n);
            store.insert(bundle, identity).unwrap();
        }
        let (bundle, identity, _) = fixture(&signer, &account, &Account::generate(), 50);
        store.insert(bundle, identity).unwrap();
        let (bundle, identity, _) = fixture(&signer, &account, &target, 24);
        assert!(!store.insert(bundle, identity).unwrap());
        profile.database().transaction(|c| {c.execute_batch("CREATE TEMP TRIGGER reject_acceptance BEFORE INSERT ON file_identity_acceptance BEGIN SELECT RAISE(ABORT,'acceptance interrupted'); END;")?;Ok(())}).unwrap();
        let (bundle, identity, _) = fixture(&signer, &account, &target, 26);
        assert!(store.insert(bundle.clone(), identity.clone()).is_err());
        assert_eq!(store.revision().unwrap(), 25);
        assert!(!store.contains(host, identity.logical).unwrap());
        profile
            .database()
            .transaction(|c| {
                c.execute_batch("DROP TRIGGER reject_acceptance")?;
                Ok(())
            })
            .unwrap();
        assert!(store.insert(bundle, identity.clone()).unwrap());
        profile
            .database()
            .transaction(|c| {
                c.execute("UPDATE file_identity_bodies SET ciphertext=zeroblob(1)", [])?;
                Ok(())
            })
            .unwrap();
        let page = store.accepted_page(host, 20, 24, 2).unwrap();
        assert_eq!(
            page.iter().map(|p| p.revision).collect::<Vec<_>>(),
            [21, 22]
        );
        let page = store.accepted_page(host, 24, 26, 128).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].revision, 26);
        assert_eq!(page[0].logical, identity.logical);
        assert_eq!(
            store.accepted_aliases(host, identity.logical).unwrap(),
            identity.aliases
        );
        for (after, through, rows) in [(0, 26, 0), (0, 26, 129), (26, 25, 1), (0, u64::MAX, 1)] {
            assert!(matches!(
                store.accepted_page(host, after, through, rows),
                Err(LogError::InvalidPageBudget)
            ));
        }
        drop(store);
        drop(profile);
        let profile = owner(root.path(), &signer, &account);
        crate::storage::record_log::frame_observations::reset();
        let store = IndexedFileMessageIdentities::from_profile(profile, &path, "pw").unwrap();
        assert_eq!(
            crate::storage::record_log::frame_observations::maximum_capacity(),
            0
        );
        assert_eq!(
            store.accepted_page(host, 24, 26, 128).unwrap()[0].logical,
            identity.logical
        );
    }

    #[test]
    fn indexed_file_identity_acceptance_upgrade_uses_metadata_baseline() {
        let root = tempfile::tempdir().unwrap();
        let signer = DeviceIdentity::generate();
        let account = Account::generate();
        let target = Account::generate();
        let profile = owner(root.path(), &signer, &account);
        let path = root.path().join("file-message-identities.log");
        let mut store =
            IndexedFileMessageIdentities::from_profile(profile.clone(), &path, "pw").unwrap();
        let (bundle, identity, _) = fixture(&signer, &account, &target, 1);
        let host = identity.host();
        store.insert(bundle, identity).unwrap();
        profile.database().transaction(|c|{c.execute_batch("DROP TABLE file_identity_acceptance; UPDATE file_identity_bodies SET ciphertext=zeroblob(1)")?;Ok(())}).unwrap();
        drop(store);
        drop(profile);
        let profile = owner(root.path(), &signer, &account);
        let mut store = IndexedFileMessageIdentities::from_profile(profile, &path, "pw").unwrap();
        assert_eq!(store.revision().unwrap(), 1);
        assert!(store.accepted_page(host, 0, 1, 128).unwrap().is_empty());
        let (bundle, identity, _) = fixture(&signer, &account, &target, 2);
        store.insert(bundle, identity).unwrap();
        assert_eq!(store.accepted_page(host, 0, 2, 128).unwrap()[0].revision, 2);
    }

    #[test]
    fn indexed_file_identity_history_append_reuses_prior_metadata() {
        use crate::discovery::Roster;
        use crate::node::transcript_repository::take_header_reads_for_test;
        use std::sync::Mutex;
        let root = tempfile::tempdir().unwrap();
        let signer = DeviceIdentity::generate();
        let secrets = signer.secret_bytes();
        let account = Account::generate();
        let target = Account::generate();
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (channel, _) = tokio::sync::mpsc::unbounded_channel();
        let (file, _) = tokio::sync::mpsc::unbounded_channel();
        let node = Node::open_with_account(
            DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
            Account::from_secret_bytes(account.secret_bytes()),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            &root.path().join("events.log"),
            &root.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        let append = |n: u8, bind: bool, proof: bool| {
            let (_, mut identity, mut row) = fixture(&signer, &account, &target, n);
            let original = Event::new(
                &signer,
                ConversationId::new([251; 32]),
                u64::from(n),
                vec![],
                u64::from(n),
                u64::from(n),
                EventKind::FileManifest,
                vec![n; 64],
            );
            row.event_id = original.id;
            row.wall_clock = original.wall_clock;
            identity.logical = if bind {
                original.id
            } else {
                EventId::new([250; 32])
            };
            identity.aliases = if bind {
                vec![original.id]
            } else {
                vec![identity.logical, original.id]
            };
            let mut body = PREFIX.to_vec();
            body.extend(bincode::serialize(&identity).unwrap());
            let bundle = Event::new(
                &signer,
                scope(identity.logical),
                1,
                vec![],
                1,
                original.wall_clock,
                EventKind::Message,
                body,
            );
            if bind {
                node.file_message_identities
                    .lock()
                    .unwrap()
                    .insert(bundle.clone(), identity.clone())
                    .unwrap();
            }
            if proof {
                node.received_files
                    .lock()
                    .unwrap()
                    .record_file_with_reception(&row, Some(&original), None)
                    .unwrap();
            } else {
                // A retained card is not authority. Later proof acceptance must
                // reproject this old ordinal even though its bytes are unchanged.
                node.received_files
                    .lock()
                    .unwrap()
                    .record_file_alias_durable(&row)
                    .unwrap();
            }
            (original, bundle, identity, row)
        };
        for n in 1..=128 {
            append(n, true, true);
        }
        assert_eq!(
            node.account_history(&target.account_id(), 50)
                .unwrap()
                .len(),
            50
        );
        take_header_reads_for_test();
        let appended = append(129, true, true).0.id;
        let recent = node.account_history(&target.account_id(), 50).unwrap();
        let examined = take_header_reads_for_test();
        assert_eq!(recent.len(), 50);
        assert_eq!(recent.last().unwrap().id, appended);
        assert_eq!(recent.first().unwrap().wall_clock, 80);
        assert!(
            examined <= 16,
            "one accepted file binding must not rescan128 historical headers (examined {examined})"
        );
        let (original, bundle, identity, _) = append(130, false, true);
        assert_eq!(
            node.account_history(&target.account_id(), 50)
                .unwrap()
                .last()
                .unwrap()
                .id,
            original.id
        );
        node.file_message_identities
            .lock()
            .unwrap()
            .insert(bundle, identity.clone())
            .unwrap();
        let retargeted = node.account_history(&target.account_id(), 50).unwrap();
        assert_eq!(retargeted.last().unwrap().id, identity.logical);
        assert!(!retargeted.iter().any(|row| row.id == original.id));

        let (original, _, _, row) = append(131, true, false);
        assert!(
            !node
                .account_history(&target.account_id(), 50)
                .unwrap()
                .iter()
                .any(|row| row.id == original.id)
        );
        node.received_files
            .lock()
            .unwrap()
            .record_file_with_reception(&row, Some(&original), None)
            .unwrap();
        assert_eq!(
            node.account_history(&target.account_id(), 50)
                .unwrap()
                .last()
                .unwrap()
                .id,
            original.id
        );
        take_header_reads_for_test();
        let next = append(132, true, true).0.id;
        assert_eq!(
            node.account_history(&target.account_id(), 50)
                .unwrap()
                .last()
                .unwrap()
                .id,
            next
        );
        assert!(
            take_header_reads_for_test() <= 16,
            "safe FILES tails resume after a required rebuild"
        );
        // Explicit retained-host history has its own visibility gate. It uses
        // the same append proof without inheriting the active account route.
        node.history(row.conversation, 50).unwrap();
        take_header_reads_for_test();
        let next = append(133, true, true).0.id;
        assert_eq!(
            node.history(row.conversation, 50)
                .unwrap()
                .last()
                .unwrap()
                .id,
            next
        );
        assert!(
            take_header_reads_for_test() <= 16,
            "explicit host history also reuses a safe FILES prefix"
        );
    }
}
