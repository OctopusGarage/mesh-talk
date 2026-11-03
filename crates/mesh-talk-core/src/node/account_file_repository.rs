//! Profile-owned, demand-read signed file proofs retained outside the event DAG.
use super::transcript_repository::{
    FILE_PROOFS, MAX_PAGE_BYTES, MAX_PAGE_ROWS, publication::PublishedHistorySource,
};
use crate::{
    eventlog::{Author, ConversationId, Event, EventId, EventKind, LogError},
    storage::{
        profile::{context::ProfileStorage, manifest},
        record_log::EncryptedRecordLog,
    },
};
use rusqlite::{Connection, params};
use std::{path::Path, sync::Arc};

const SCHEMA: &str = "
CREATE TABLE account_file_proof_revision(singleton INTEGER PRIMARY KEY CHECK(singleton=1), revision INTEGER NOT NULL CHECK(revision>=0));
CREATE TABLE account_file_proof_headers(id BLOB PRIMARY KEY CHECK(length(id)=32), conversation BLOB NOT NULL CHECK(length(conversation)=32), author BLOB NOT NULL CHECK(length(author)=32), sequence BLOB NOT NULL CHECK(length(sequence)=8), lamport BLOB NOT NULL CHECK(length(lamport)=8), clock BLOB NOT NULL CHECK(length(clock)=8), parent_bytes INTEGER NOT NULL CHECK(parent_bytes>=0 AND parent_bytes%32=0), ciphertext_bytes INTEGER NOT NULL CHECK(ciphertext_bytes>=0), signature_bytes INTEGER NOT NULL CHECK(signature_bytes>=0), allocated_bytes INTEGER NOT NULL CHECK(allocated_bytes>0)) WITHOUT ROWID;
CREATE TABLE account_file_proof_bodies(id BLOB PRIMARY KEY REFERENCES account_file_proof_headers(id) ON DELETE CASCADE, parents BLOB NOT NULL, ciphertext BLOB NOT NULL, signature BLOB NOT NULL) WITHOUT ROWID;
";
fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}
fn validate(event: &Event) -> Result<(), LogError> {
    if event.kind != EventKind::FileManifest
        || !event.is_canonical()
        || !event.verify_integrity()
        || !event.verify_signature()
    {
        return Err(invalid("invalid file proof"));
    }
    Ok(())
}
fn charge(parents: usize, ciphertext: usize, signature: usize) -> Result<usize, LogError> {
    std::mem::size_of::<Event>()
        .checked_add(parents)
        .and_then(|n| n.checked_add(ciphertext))
        .and_then(|n| n.checked_add(signature))
        .filter(|n| *n <= MAX_PAGE_BYTES)
        .ok_or_else(|| invalid("file proof exceeds payload budget"))
}
#[cfg(test)]
thread_local! {static BODY_FETCHES:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
pub(crate) struct IndexedAccountFileProofs {
    source: PublishedHistorySource,
}
impl IndexedAccountFileProofs {
    pub(crate) fn authenticate(
        profile: &ProfileStorage,
        path: &Path,
        password: &str,
    ) -> Result<(), LogError> {
        PublishedHistorySource::authenticate_legacy_with::<Event>(
            profile,
            path,
            password,
            FILE_PROOFS,
            b"MTAFPR",
            |event| validate(&event),
        )
    }
    pub(crate) fn revision(&self) -> Result<u64, LogError> {
        self.source.profile().database().read(|c| {
            let revision: i64 = c.query_row(
                "SELECT revision FROM account_file_proof_revision WHERE singleton=1",
                [],
                |row| row.get(0),
            )?;
            u64::try_from(revision).map_err(|_| invalid("negative file proof revision"))
        })
    }
    pub(crate) fn from_profile(
        profile: Arc<ProfileStorage>,
        path: &Path,
        password: &str,
    ) -> Result<Self, LogError> {
        profile.participant_database().ok_or_else(||invalid("file proof profile is not initialized"))?.transaction(|c| {
            let registry: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='transcript_sources')",[],|r|r.get(0))?;
            let imported = registry && c.query_row("SELECT EXISTS(SELECT 1 FROM transcript_sources WHERE kind=?1)",[FILE_PROOFS],|r|r.get::<_,bool>(0))?;
            let present:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='account_file_proof_headers')",[],|r|r.get(0))?;
            if !present && !imported {c.execute_batch(SCHEMA)?;c.execute("INSERT INTO account_file_proof_revision VALUES(1,0)",[])?;}
            manifest::validate_ddl(c,SCHEMA)
        })?;
        let source = PublishedHistorySource::open(
            profile,
            path,
            password,
            FILE_PROOFS,
            |c, snapshot, password| {
                EncryptedRecordLog::<Event>::open_with_visitor(
                    snapshot,
                    password,
                    b"MTAFPR",
                    |event| Self::put_on(c, &event),
                )?;
                Ok(())
            },
        )?;
        Ok(Self { source })
    }
    pub(crate) fn get(&self, id: &EventId) -> Result<Option<Event>, LogError> {
        self.source
            .profile()
            .database()
            .read(|c| Self::get_on(c, id))
    }
    pub(crate) fn get_on(c: &Connection, id: &EventId) -> Result<Option<Event>, LogError> {
        let mut statement=c.prepare_cached("SELECT conversation,author,sequence,lamport,clock,parent_bytes,ciphertext_bytes,signature_bytes,allocated_bytes FROM account_file_proof_headers WHERE id=?1")?;
        let mut rows = statement.query([id.as_bytes().as_slice()])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        fn blob<const N: usize>(row: &rusqlite::Row<'_>, i: usize) -> Result<[u8; N], LogError> {
            row.get_ref(i)?
                .as_blob()
                .map_err(|_| invalid("invalid proof metadata"))?
                .try_into()
                .map_err(|_| invalid("invalid proof metadata width"))
        }
        let conversation_id = ConversationId::new(blob(row, 0)?);
        let author = Author::from_ed25519(blob(row, 1)?);
        let seq = u64::from_be_bytes(blob(row, 2)?);
        let lamport = u64::from_be_bytes(blob(row, 3)?);
        let wall_clock = u64::from_be_bytes(blob(row, 4)?);
        let size = |i| -> Result<usize, LogError> {
            usize::try_from(row.get::<_, i64>(i)?).map_err(|_| invalid("invalid proof size"))
        };
        let parents = size(5)?;
        let ciphertext = size(6)?;
        let signature = size(7)?;
        if parents % 32 != 0 || charge(parents, ciphertext, signature)? != size(8)? {
            return Err(invalid("file proof charge disagrees with metadata"));
        }
        // SQLite can materialize a BLOB while stepping its SELECT, before
        // get_ref exposes the bytes. Validate physical lengths with scalar reads.
        let lengths: (i64, i64, i64) = c.prepare_cached(
            "SELECT length(parents),length(ciphertext),length(signature) FROM account_file_proof_bodies WHERE id=?1"
        )?.query_row([id.as_bytes().as_slice()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        if lengths != (parents as i64, ciphertext as i64, signature as i64) {
            return Err(invalid("file proof body disagrees with metadata"));
        }
        #[cfg(test)]
        BODY_FETCHES.with(|count| count.set(count.get() + 1));
        let mut body = c.prepare_cached(
            "SELECT parents,ciphertext,signature FROM account_file_proof_bodies WHERE id=?1",
        )?;
        let mut body_rows = body.query([id.as_bytes().as_slice()])?;
        let body_row = body_rows
            .next()?
            .ok_or_else(|| invalid("file proof body missing"))?;
        let read = |i| {
            body_row
                .get_ref(i)?
                .as_blob()
                .map_err(|_| invalid("invalid file proof body"))
        };
        let parent_data = read(0)?;
        let ciphertext_data = read(1)?;
        let signature_data = read(2)?;
        if parent_data.len() != parents
            || ciphertext_data.len() != ciphertext
            || signature_data.len() != signature
        {
            return Err(invalid("file proof body disagrees with metadata"));
        }
        let event = Event {
            id: *id,
            conversation_id,
            author,
            seq,
            parents: parent_data
                .as_chunks::<32>()
                .0
                .iter()
                .map(|chunk| EventId::new(*chunk))
                .collect(),
            lamport,
            wall_clock,
            kind: EventKind::FileManifest,
            ciphertext: ciphertext_data.to_vec(),
            sig: signature_data.to_vec(),
        };
        validate(&event)?;
        Ok(Some(event))
    }
    fn put_on(c: &Connection, event: &Event) -> Result<(), LogError> {
        validate(event)?;
        let parent_bytes = event
            .parents
            .len()
            .checked_mul(32)
            .ok_or_else(|| invalid("proof parent size overflow"))?;
        let allocated = charge(parent_bytes, event.ciphertext.len(), event.sig.len())?;
        if let Some(old) = Self::get_on(c, &event.id)? {
            if old != *event {
                return Err(invalid("conflicting file proof"));
            }
            return Ok(());
        }
        c.execute(
            "INSERT INTO account_file_proof_headers VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                event.id.as_bytes().as_slice(),
                event.conversation_id.as_bytes().as_slice(),
                event.author.ed25519_pub().as_slice(),
                event.seq.to_be_bytes().as_slice(),
                event.lamport.to_be_bytes().as_slice(),
                event.wall_clock.to_be_bytes().as_slice(),
                parent_bytes as i64,
                event.ciphertext.len() as i64,
                event.sig.len() as i64,
                allocated as i64
            ],
        )?;
        let mut parents = Vec::with_capacity(parent_bytes);
        for parent in &event.parents {
            parents.extend_from_slice(parent.as_bytes());
        }
        c.execute(
            "INSERT INTO account_file_proof_bodies VALUES(?1,?2,?3,?4)",
            params![
                event.id.as_bytes().as_slice(),
                parents,
                &event.ciphertext,
                &event.sig
            ],
        )?;
        if c.execute("UPDATE account_file_proof_revision SET revision=revision+1 WHERE singleton=1 AND revision<9223372036854775807",[])? != 1 { return Err(invalid("file proof revision exhausted")); }
        Ok(())
    }
    pub(crate) fn insert(&mut self, event: Event) -> Result<(), LogError> {
        self.source.check_write()?;
        self.source
            .profile()
            .database()
            .transaction(|c| Self::put_on(c, &event))
    }
    pub(crate) fn page(
        &self,
        after: Option<EventId>,
        rows: usize,
        bytes: usize,
    ) -> Result<Vec<Event>, LogError> {
        if rows == 0 || rows > MAX_PAGE_ROWS || bytes == 0 || bytes > MAX_PAGE_BYTES {
            return Err(LogError::InvalidPageBudget);
        }
        self.source.profile().database().read(|c| {
            let mut statement=c.prepare_cached("SELECT id,allocated_bytes FROM account_file_proof_headers WHERE id>?1 ORDER BY id LIMIT ?2")?;
            let mut selected=statement.query(params![after.as_ref().map(|id|id.as_bytes().as_slice()).unwrap_or(&[]),rows as i64])?;
            let mut ids=Vec::new();let mut total=0usize;
            while let Some(row)=selected.next()? {
                let n=usize::try_from(row.get::<_,i64>(1)?).map_err(|_|invalid("invalid proof allocation"))?;
                let next=total.checked_add(n).ok_or_else(||invalid("proof page allocation overflow"))?;
                if next>bytes {if ids.is_empty(){return Err(LogError::InvalidPageBudget);}break;}
                let raw=row.get_ref(0)?.as_blob().map_err(|_|invalid("invalid proof ID"))?;
                ids.push(EventId::new(raw.try_into().map_err(|_|invalid("invalid proof ID width"))?));total=next;
            }
            let mut result = Vec::with_capacity(ids.len());
            for id in ids {
                result.push(Self::get_on(c,&id)?.ok_or_else(||invalid("selected file proof disappeared"))?);
            }
            Ok(result)
        })
    }
    /// Drop the SQL guard before callbacks so export can consult other owners.
    pub(crate) fn visit(
        &self,
        mut visit: impl FnMut(Event) -> Result<(), LogError>,
    ) -> Result<(), LogError> {
        let mut cursor = None;
        loop {
            let rows = self.page(cursor, MAX_PAGE_ROWS, MAX_PAGE_BYTES)?;
            if rows.is_empty() {
                return Ok(());
            }
            for event in rows {
                cursor = Some(event.id);
                visit(event)?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::account_file_proofs::AccountFileProofs;
    use super::*;
    use crate::{
        eventlog::{ConversationId, EventKind},
        identity::{account::Account, device::DeviceIdentity},
        storage::profile::ProfileStorageLease,
    };
    fn profile(root: &Path, signer: &DeviceIdentity, account: &Account) -> Arc<ProfileStorage> {
        let profile = ProfileStorage::open_participant(
            ProfileStorageLease::acquire(root).unwrap(),
            &signer.public(),
            &account.public(),
            &root.join("messages.log"),
            &root.join("sent.log"),
            "pw",
        )
        .unwrap();
        profile.complete_participant("pw", None).unwrap();
        profile
    }
    #[test]
    fn file_proof_warm_open_does_not_decode_retained_bodies() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("account-file-proofs.log");
        let signer = DeviceIdentity::generate();
        let account = Account::generate();
        let mut legacy = AccountFileProofs::open(&path, "pw").unwrap();
        let mut last = None;
        for index in 0..80 {
            let proof = Event::new(
                &signer,
                ConversationId::new([1; 32]),
                index + 1,
                vec![],
                index + 1,
                index,
                EventKind::FileManifest,
                vec![42; 4096],
            );
            legacy.insert(proof.clone()).unwrap();
            last = Some(proof);
        }
        drop(legacy);
        let expected = last.unwrap();
        let owner = profile(directory.path(), &signer, &account);
        let first = IndexedAccountFileProofs::from_profile(owner.clone(), &path, "pw").unwrap();
        assert_eq!(first.revision().unwrap(), 80);
        assert_eq!(first.get(&expected.id).unwrap(), Some(expected.clone()));
        drop(first);
        drop(owner);
        let owner = profile(directory.path(), &signer, &account);
        crate::storage::record_log::frame_observations::reset();
        let mut reopened = IndexedAccountFileProofs::from_profile(owner, &path, "pw").unwrap();
        assert_eq!(
            crate::storage::record_log::frame_observations::maximum_capacity(),
            0,
            "ordinary proof open must not read historical legacy frames"
        );
        assert_eq!(reopened.get(&expected.id).unwrap(), Some(expected.clone()));
        reopened.insert(expected).unwrap();
        assert_eq!(reopened.revision().unwrap(), 80);
        assert_eq!(&std::fs::read(path).unwrap()[..6], b"MTSQL2");
    }
    fn proof(signer: &DeviceIdentity, sequence: u64) -> Event {
        Event::new(
            signer,
            ConversationId::new([3; 32]),
            sequence,
            vec![],
            sequence,
            sequence,
            EventKind::FileManifest,
            vec![42; 4096],
        )
    }
    #[test]
    fn proof_pages_respect_count_and_allocation_and_allow_reentrant_export() {
        let directory = tempfile::tempdir().unwrap();
        let signer = DeviceIdentity::generate();
        let account = Account::generate();
        let owner = profile(directory.path(), &signer, &account);
        let mut store = IndexedAccountFileProofs::from_profile(
            owner,
            directory.path().join("account-file-proofs.log").as_path(),
            "pw",
        )
        .unwrap();
        for n in 0..7 {
            store.insert(proof(&signer, n)).unwrap();
        }
        let first = store.page(None, 3, MAX_PAGE_BYTES).unwrap();
        assert_eq!(first.len(), 3);
        let one = charge(0, 4096, 64).unwrap();
        assert_eq!(store.page(None, 128, one * 2).unwrap().len(), 2);
        assert!(store.page(None, 129, MAX_PAGE_BYTES).is_err());
        assert!(store.page(None, 1, one - 1).is_err());
        let mut ids = Vec::new();
        store
            .visit(|event| {
                assert_eq!(store.get(&event.id)?, Some(event.clone()));
                ids.push(event.id);
                Ok(())
            })
            .unwrap();
        assert_eq!(ids.len(), 7);
        assert!(
            ids.windows(2)
                .all(|pair| pair[0].as_bytes() < pair[1].as_bytes())
        );
        assert_eq!(
            store
                .page(ids.last().copied(), 128, MAX_PAGE_BYTES)
                .unwrap()
                .len(),
            0
        );
    }
    #[test]
    fn proof_writes_are_exact_and_require_the_published_barrier() {
        let directory = tempfile::tempdir().unwrap();
        let signer = DeviceIdentity::generate();
        let account = Account::generate();
        let owner = profile(directory.path(), &signer, &account);
        let path = directory.path().join("account-file-proofs.log");
        let mut store = IndexedAccountFileProofs::from_profile(owner, &path, "pw").unwrap();
        let event = proof(&signer, 1);
        store.insert(event.clone()).unwrap();
        store.insert(event.clone()).unwrap();
        let mut tampered = event.clone();
        tampered.ciphertext[0] ^= 1;
        assert!(store.insert(tampered).is_err());
        assert_eq!(store.page(None, 128, MAX_PAGE_BYTES).unwrap().len(), 1);
        std::fs::write(path, b"changed").unwrap();
        assert!(store.insert(proof(&signer, 2)).is_err());
        assert_eq!(store.get(&event.id).unwrap(), Some(event));
    }
    #[test]
    fn corrupt_proof_import_preserves_source_and_rolls_back_rows() {
        let directory = tempfile::tempdir().unwrap();
        let signer = DeviceIdentity::generate();
        let account = Account::generate();
        let path = directory.path().join("account-file-proofs.log");
        let (mut legacy, _) = EncryptedRecordLog::<Event>::open(&path, "pw", b"MTAFPR").unwrap();
        legacy.append_durable(&proof(&signer, 1)).unwrap();
        let mut wrong = proof(&signer, 2);
        wrong.kind = EventKind::Message;
        legacy.append_durable(&wrong).unwrap();
        drop(legacy);
        let original = std::fs::read(&path).unwrap();
        let owner = profile(directory.path(), &signer, &account);
        assert!(IndexedAccountFileProofs::from_profile(owner.clone(), &path, "pw").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let count: i64 = owner
            .database()
            .read(|c| {
                Ok(
                    c.query_row("SELECT count(*) FROM account_file_proof_headers", [], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(count, 0);
    }
    #[test]
    fn proof_header_corruption_fails_before_oversized_body_allocation() {
        let directory = tempfile::tempdir().unwrap();
        let signer = DeviceIdentity::generate();
        let account = Account::generate();
        let owner = profile(directory.path(), &signer, &account);
        let path = directory.path().join("account-file-proofs.log");
        let mut store = IndexedAccountFileProofs::from_profile(owner.clone(), &path, "pw").unwrap();
        let event = proof(&signer, 1);
        store.insert(event.clone()).unwrap();
        owner
            .database()
            .transaction(|c| {
                c.execute(
                    "UPDATE account_file_proof_headers SET ciphertext_bytes=?1,allocated_bytes=?2",
                    params![MAX_PAGE_BYTES as i64, (MAX_PAGE_BYTES + 4096) as i64],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(store.get(&event.id).is_err());
        assert!(store.page(None, 128, MAX_PAGE_BYTES).is_err());
    }
    #[test]
    fn corrupt_body_lengths_are_rejected_before_payload_fetch() {
        let directory = tempfile::tempdir().unwrap();
        let signer = DeviceIdentity::generate();
        let account = Account::generate();
        let owner = profile(directory.path(), &signer, &account);
        let path = directory.path().join("account-file-proofs.log");
        let mut store = IndexedAccountFileProofs::from_profile(owner.clone(), &path, "pw").unwrap();
        let event = proof(&signer, 1);
        store.insert(event.clone()).unwrap();
        owner
            .database()
            .transaction(|c| {
                c.execute(
                    "UPDATE account_file_proof_bodies SET ciphertext=zeroblob(?1)",
                    [(MAX_PAGE_BYTES + 1) as i64],
                )?;
                Ok(())
            })
            .unwrap();
        BODY_FETCHES.with(|count| count.set(0));
        assert!(store.get(&event.id).is_err());
        assert_eq!(
            BODY_FETCHES.with(|count| count.get()),
            0,
            "corrupt payload must not reach BLOB fetch"
        );
    }
    #[test]
    fn proof_page_owns_only_admitted_event_slots() {
        let directory = tempfile::tempdir().unwrap();
        let signer = DeviceIdentity::generate();
        let account = Account::generate();
        let owner = profile(directory.path(), &signer, &account);
        let path = directory.path().join("account-file-proofs.log");
        let mut store = IndexedAccountFileProofs::from_profile(owner, &path, "pw").unwrap();
        for n in 0..7 {
            store.insert(proof(&signer, n)).unwrap();
        }
        let rows = store
            .page(None, 7, charge(0, 4096, 64).unwrap() * 7)
            .unwrap();
        assert_eq!(rows.len(), 7);
        assert_eq!(rows.capacity(), 7, "uncharged spare Event slots");
    }
}
