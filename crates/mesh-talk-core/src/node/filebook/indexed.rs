//! Account-scoped file bindings and exact processing evidence on the participant
//! connection. Payloads stay in the existing FILES transcript source.
use crate::{
    eventlog::{ConversationId, Event, EventId, LogError},
    file::AnyManifest,
    node::received_log::ReceivedEntry,
    storage::profile::context::ProfileStorage,
};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::sync::Arc;

const SCHEMA: &str = "
CREATE TABLE file_book_owners(account BLOB PRIMARY KEY CHECK(length(account)=32), after_ordinal INTEGER CHECK(after_ordinal IS NULL OR after_ordinal>0), imported INTEGER NOT NULL CHECK(imported IN (0,1)), revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=0)) WITHOUT ROWID;
CREATE TABLE file_book_bindings(account BLOB NOT NULL CHECK(length(account)=32), file BLOB NOT NULL CHECK(length(file)=32), digest BLOB NOT NULL CHECK(length(digest)=32), offers INTEGER NOT NULL CHECK(typeof(offers)='integer' AND offers>=0), PRIMARY KEY(account,file)) WITHOUT ROWID;
CREATE INDEX file_book_live_files ON file_book_bindings(account,file) WHERE offers>0;
CREATE TABLE file_book_offers(account BLOB NOT NULL CHECK(length(account)=32), ordinal INTEGER NOT NULL REFERENCES transcript_rows(ordinal) ON DELETE CASCADE, file BLOB NOT NULL CHECK(length(file)=32), event BLOB NOT NULL CHECK(length(event)=32), parent BLOB NOT NULL CHECK(length(parent)=32), author BLOB NOT NULL CHECK(length(author)=32), PRIMARY KEY(account,ordinal), FOREIGN KEY(account,file) REFERENCES file_book_bindings(account,file)) WITHOUT ROWID;
CREATE INDEX file_book_file_offers ON file_book_offers(account,file,ordinal);
CREATE INDEX file_book_author_offers ON file_book_offers(account,author,ordinal);
CREATE INDEX file_book_event_offers ON file_book_offers(account,event,ordinal);
CREATE TABLE file_book_processed(account BLOB NOT NULL CHECK(length(account)=32), event BLOB NOT NULL CHECK(length(event)=32), PRIMARY KEY(account,event)) WITHOUT ROWID;
";
const TRIGGERS: &[(&str, &str)] = &[
    (
        "file_book_offer_added",
        "CREATE TRIGGER file_book_offer_added AFTER INSERT ON file_book_offers BEGIN UPDATE file_book_bindings SET offers=offers+1 WHERE account=NEW.account AND file=NEW.file; UPDATE file_book_owners SET revision=revision+1 WHERE account=NEW.account; END",
    ),
    (
        "file_book_offer_removed",
        "CREATE TRIGGER file_book_offer_removed AFTER DELETE ON file_book_offers BEGIN UPDATE file_book_bindings SET offers=offers-1 WHERE account=OLD.account AND file=OLD.file; UPDATE file_book_owners SET revision=revision+1 WHERE account=OLD.account; END",
    ),
    (
        "file_book_suppressed",
        "CREATE TRIGGER file_book_suppressed AFTER INSERT ON transcript_suppression BEGIN DELETE FROM file_book_offers WHERE event=NEW.id AND ordinal IN (SELECT ordinal FROM transcript_rows WHERE kind=3 AND conversation=NEW.conversation AND row_key=NEW.id); DELETE FROM file_book_offers WHERE ordinal IN (SELECT o.ordinal FROM file_identity_headers h JOIN file_identity_aliases a ON a.host=h.host AND a.logical=h.logical JOIN file_book_offers o ON o.event=a.manifest AND o.author=h.author AND o.file=h.file JOIN transcript_rows r ON r.ordinal=o.ordinal AND r.plaintext_sha256=h.manifest_digest WHERE h.host=NEW.conversation AND (h.logical=NEW.id OR a.manifest=NEW.id)); END",
    ),
    (
        "file_book_suppressed_alias",
        "CREATE TRIGGER file_book_suppressed_alias AFTER INSERT ON file_identity_aliases BEGIN DELETE FROM file_book_offers WHERE event=NEW.manifest AND ordinal IN (SELECT o.ordinal FROM file_book_offers o JOIN transcript_rows r ON r.ordinal=o.ordinal JOIN file_identity_headers h ON h.host=NEW.host AND h.logical=NEW.logical AND h.author=o.author AND h.file=o.file AND h.manifest_digest=r.plaintext_sha256 WHERE o.event=NEW.manifest AND EXISTS(SELECT 1 FROM transcript_suppression s WHERE s.conversation=NEW.host AND (s.id=NEW.logical OR s.id=NEW.manifest))); END",
    ),
];
const MAX_BODY: usize = crate::node::transcript_repository::MAX_PAGE_BYTES;
fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}
fn blob<const N: usize>(row: &rusqlite::Row<'_>, index: usize) -> Result<[u8; N], LogError> {
    row.get_ref(index)?
        .as_blob()
        .map_err(|_| invalid("invalid file-book metadata"))?
        .try_into()
        .map_err(|_| invalid("invalid file-book metadata width"))
}
fn active(c: &Connection) -> Result<[u8; 32], LogError> {
    let mut statement =
        c.prepare_cached("SELECT account FROM participant_profile WHERE singleton=1")?;
    let mut rows = statement.query([])?;
    blob(
        rows.next()?
            .ok_or_else(|| invalid("missing file-book authority"))?,
        0,
    )
}
fn digest(bytes: &[u8]) -> Result<[u8; 32], LogError> {
    crate::node::files::validated_manifest(bytes)
        .as_ref()
        .and_then(crate::node::files::file_transfer_fingerprint)
        .ok_or_else(|| invalid("invalid file transfer binding"))
}

fn page_limit(rows: usize, bytes: usize, charge: usize) -> Result<i64, LogError> {
    if rows == 0 || rows > 128 || bytes == 0 || bytes > MAX_BODY {
        return Err(LogError::InvalidPageBudget);
    }
    if bytes < charge {
        return Err(LogError::PageBudgetTooSmall { required: charge });
    }
    Ok(rows.min(bytes / charge) as i64)
}

pub(crate) struct IndexedFileBook {
    profile: Arc<ProfileStorage>,
    account: [u8; 32],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileOffer {
    pub ordinal: i64,
    pub event: EventId,
    pub file: ConversationId,
    pub parent: ConversationId,
    pub author: crate::eventlog::Author,
}
impl IndexedFileBook {
    pub(crate) fn revision(&self) -> Result<u64, LogError> {
        self.profile.database().read(|c| {
            self.check(c)?;
            let revision: i64 = c.query_row(
                "SELECT revision FROM file_book_owners WHERE account=?1",
                [self.account.as_slice()],
                |r| r.get(0),
            )?;
            u64::try_from(revision).map_err(|_| invalid("invalid file eligibility revision"))
        })
    }
    pub(crate) fn eligible_ordinal(&self, ordinal: i64) -> Result<bool, LogError> {
        self.profile.database().read(|c| {
            self.check(c)?;
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM file_book_offers WHERE account=?1 AND ordinal=?2)",
                params![self.account.as_slice(), ordinal],
                |r| r.get(0),
            )?)
        })
    }
    pub(crate) fn files_page(
        &self,
        after: Option<ConversationId>,
        rows: usize,
        bytes: usize,
    ) -> Result<Vec<ConversationId>, LogError> {
        let limit = page_limit(rows, bytes, 32)?;
        self.profile.database().read(|c| {
            self.check(c)?;
            let mut statement = c.prepare_cached("SELECT file FROM file_book_bindings WHERE account=?1 AND offers>0 AND file>coalesce(?2,x'') ORDER BY file LIMIT ?3")?;
            let mut rows = statement.query(params![self.account.as_slice(),after.as_ref().map(|id|id.as_bytes().as_slice()),limit])?;
            let mut result = Vec::new();
            while let Some(row) = rows.next()? { result.push(ConversationId::new(blob(row,0)?)); }
            Ok(result)
        })
    }
    pub(crate) fn offers_page(
        &self,
        file: Option<ConversationId>,
        author: Option<crate::eventlog::Author>,
        after: Option<i64>,
        rows: usize,
        bytes: usize,
    ) -> Result<Vec<FileOffer>, LogError> {
        let limit = page_limit(rows, bytes, 144)?;
        self.profile.database().read(|c| {
            self.check(c)?;
            // Distinct statements preserve key seeks on each compound index.
            let selector = match (file,author) { (Some(_),Some(_)) => " AND file=?4 AND author=?5", (Some(_),None) => " AND file=?4", (None,Some(_)) => " AND author=?5", (None,None) => "" };
            let mut statement = c.prepare_cached(&format!("SELECT ordinal,event,file,parent,author FROM file_book_offers WHERE account=?1 AND ordinal>?2{selector} ORDER BY ordinal LIMIT ?3"))?;
            statement.raw_bind_parameter(1,self.account.as_slice())?;
            statement.raw_bind_parameter(2,after.unwrap_or(0))?;
            statement.raw_bind_parameter(3,limit)?;
            if let Some(file)=file { statement.raw_bind_parameter(4,file.as_bytes().as_slice())?; }
            if let Some(author)=author { statement.raw_bind_parameter(5,author.ed25519_pub().as_slice())?; }
            let mut rows = statement.raw_query();
            let mut result = Vec::new();
            while let Some(row)=rows.next()? { result.push(FileOffer { ordinal:row.get(0)?,event:EventId::new(blob(row,1)?),file:ConversationId::new(blob(row,2)?),parent:ConversationId::new(blob(row,3)?),author:crate::eventlog::Author::from_ed25519(blob(row,4)?) }); }
            Ok(result)
        })
    }
    /// None means migration is complete; Some(None) starts the first bounded turn.
    pub(crate) fn import_position(&self) -> Result<Option<Option<i64>>, LogError> {
        self.profile.database().read(|c| {
            self.check(c)?;
            let (after, done): (Option<i64>, bool) = c.query_row(
                "SELECT after_ordinal,imported FROM file_book_owners WHERE account=?1",
                [self.account.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Ok((!done).then_some(after))
        })
    }
    /// Call inside the same transaction as the accepted migration row. Erased
    /// or invalid rows advance only this checkpoint, never processing evidence.
    pub(crate) fn advance_import_on(
        &self,
        c: &Connection,
        before: Option<i64>,
        after: Option<i64>,
    ) -> Result<(), LogError> {
        self.check(c)?;
        if after.is_some_and(|next| next <= before.unwrap_or(0)) {
            return Err(LogError::StaleCursor);
        }
        let changed = c.execute("UPDATE file_book_owners SET after_ordinal=coalesce(?1,after_ordinal),imported=?2 WHERE account=?3 AND imported=0 AND after_ordinal IS ?4", params![after, i64::from(after.is_none()), self.account.as_slice(), before])?;
        if changed != 1 {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }
    pub(crate) fn open(profile: Arc<ProfileStorage>) -> Result<Self, LogError> {
        let account = profile.database().transaction(|c| {
            let present: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='file_book_owners')",
                [],
                |r| r.get(0),
            )?;
            if !present {
                c.execute_batch(SCHEMA)?;
                for (_, sql) in TRIGGERS {
                    c.execute_batch(sql)?;
                }
            }
            crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
            for (name, sql) in TRIGGERS {
                let actual: Option<String> = c
                    .query_row(
                        "SELECT sql FROM sqlite_master WHERE type='trigger' AND name=?1",
                        [name],
                        |r| r.get(0),
                    )
                    .optional()?;
                if actual.as_deref() != Some(*sql) {
                    return Err(invalid("incompatible file-book deletion projection"));
                }
            }
            let account = active(c)?;
            // Only the initial account may import the old unnamespaced source.
            // A newly adopted account starts with no offers.
            c.execute(
                "INSERT OR IGNORE INTO file_book_owners VALUES(?1,NULL,?2,0)",
                params![account.as_slice(), i64::from(present)],
            )?;
            Ok(account)
        })?;
        Ok(Self { profile, account })
    }
    fn check(&self, c: &Connection) -> Result<(), LogError> {
        if active(c)? == self.account {
            Ok(())
        } else {
            Err(LogError::StaleCursor)
        }
    }
    pub(crate) fn manifest(&self, file: ConversationId) -> Result<Option<AnyManifest>, LogError> {
        self.profile.database().read(|c| {
            self.check(c)?;
            manifest_on(c, &self.account, file)
        })
    }
    pub(crate) fn processed(&self, event: EventId) -> Result<bool, LogError> {
        self.profile.database().read(|c| {
            self.check(c)?;
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM file_book_processed WHERE account=?1 AND event=?2)",
                params![self.account.as_slice(), event.as_bytes().as_slice()],
                |r| r.get(0),
            )?)
        })
    }
}
fn manifest_on(
    c: &Connection,
    account: &[u8; 32],
    file: ConversationId,
) -> Result<Option<AnyManifest>, LogError> {
    let mut statement=c.prepare_cached("SELECT o.ordinal,r.body_bytes,length(b.plaintext),r.plaintext_sha256,f.digest FROM file_book_offers o JOIN transcript_rows r ON r.ordinal=o.ordinal JOIN transcript_bodies b ON b.ordinal=o.ordinal JOIN file_book_bindings f ON f.account=o.account AND f.file=o.file WHERE o.account=?1 AND o.file=?2 ORDER BY o.ordinal LIMIT 1")?;
    let mut rows = statement.query(params![account.as_slice(), file.as_bytes().as_slice()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let ordinal: i64 = row.get(0)?;
    let declared: i64 = row.get(1)?;
    let physical: i64 = row.get(2)?;
    let source_digest: [u8; 32] = blob(row, 3)?;
    let binding_digest: [u8; 32] = blob(row, 4)?;
    if declared <= 0
        || declared != physical
        || usize::try_from(declared)
            .ok()
            .is_none_or(|bytes| bytes > MAX_BODY)
    {
        return Err(invalid("invalid file manifest allocation charge"));
    }
    drop(rows);
    let body: Vec<u8> = c.query_row(
        "SELECT plaintext FROM transcript_bodies WHERE ordinal=?1",
        [ordinal],
        |r| r.get(0),
    )?;
    if <[u8; 32]>::from(Sha256::digest(&body)) != source_digest || digest(&body)? != binding_digest
    {
        return Err(invalid("file binding disagrees with retained body"));
    }
    let manifest = crate::node::files::validated_manifest(&body)
        .ok_or_else(|| invalid("invalid retained file manifest"))?;
    if manifest.file_conv() != file {
        return Err(invalid("file binding changed conversation"));
    }
    Ok(Some(manifest))
}
pub(crate) fn mark_processed_on(c: &Connection, event: EventId) -> Result<(), LogError> {
    let account = active(c)?;
    c.execute(
        "INSERT OR IGNORE INTO file_book_processed VALUES(?1,?2)",
        params![account.as_slice(), event.as_bytes().as_slice()],
    )?;
    Ok(())
}
/// The caller has already installed the exact FILES body in this transaction.
/// Retained signed evidence is mandatory; this projection introduces no new WAL.
pub(crate) fn install_on(
    c: &Connection,
    entry: &ReceivedEntry,
    original: &Event,
) -> Result<bool, LogError> {
    let account = active(c)?;
    let manifest = crate::node::files::eligible_manifest_row(entry, Some(original), None)
        .ok_or_else(|| {
            LogError::AdmissionRejected("invalid retained signed file binding".into())
        })?;
    if crate::node::file_message_identity::IndexedFileMessageIdentities::row_suppressed_on(
        c, entry,
    )? {
        mark_processed_on(c, entry.event_id)?;
        return Ok(false);
    }
    let binding = digest(&entry.plaintext)?;
    let old: Option<Vec<u8>> = c
        .query_row(
            "SELECT digest FROM file_book_bindings WHERE account=?1 AND file=?2",
            params![
                account.as_slice(),
                manifest.file_conv().as_bytes().as_slice()
            ],
            |r| r.get(0),
        )
        .optional()?;
    if old.as_deref().is_some_and(|old| old != binding) {
        return Err(LogError::AdmissionRejected(
            "conflicting file conversation binding".into(),
        ));
    }
    let mut statement=c.prepare_cached("SELECT ordinal,plaintext_sha256 FROM transcript_rows WHERE kind=3 AND conversation=?1 AND row_key=?2")?;
    let mut rows = statement.query(params![
        entry.conversation.as_bytes().as_slice(),
        entry.event_id.as_bytes().as_slice()
    ])?;
    let row = rows
        .next()?
        .ok_or_else(|| invalid("file binding has no durable body"))?;
    let ordinal: i64 = row.get(0)?;
    if blob::<32>(row, 1)? != <[u8; 32]>::from(Sha256::digest(&entry.plaintext)) {
        return Err(invalid("file binding source disagrees with body"));
    }
    drop(rows);
    c.execute(
        "INSERT OR IGNORE INTO file_book_bindings VALUES(?1,?2,?3,0)",
        params![
            account.as_slice(),
            manifest.file_conv().as_bytes().as_slice(),
            binding.as_slice()
        ],
    )?;
    let fresh = c.execute(
        "INSERT OR IGNORE INTO file_book_offers VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            account.as_slice(),
            ordinal,
            manifest.file_conv().as_bytes().as_slice(),
            entry.event_id.as_bytes().as_slice(),
            original.conversation_id.as_bytes().as_slice(),
            original.author.ed25519_pub().as_slice()
        ],
    )? != 0;
    mark_processed_on(c, entry.event_id)?;
    Ok(fresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        eventlog::EventKind,
        identity::{account::Account, device::DeviceIdentity},
        node::{
            received_store::ReceivedStore,
            transcript_repository::{FILES, TranscriptRepository},
        },
        storage::profile::ProfileStorageLease,
    };

    struct Fixture {
        root: tempfile::TempDir,
        device: DeviceIdentity,
        account: Account,
        book: IndexedFileBook,
        records: ReceivedStore,
    }
    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let device = DeviceIdentity::generate();
            let account = Account::generate();
            let (book, records) = Self::open(root.path(), &device, &account);
            Self {
                root,
                device,
                account,
                book,
                records,
            }
        }
        fn open(
            path: &std::path::Path,
            device: &DeviceIdentity,
            account: &Account,
        ) -> (IndexedFileBook, ReceivedStore) {
            let profile = ProfileStorage::open_participant(
                ProfileStorageLease::acquire(path).unwrap(),
                &device.public(),
                &account.public(),
                &path.join("messages.log"),
                &path.join("sent.log"),
                "pw",
            )
            .unwrap();
            profile.complete_participant("pw", None).unwrap();
            let records = ReceivedStore::file_cards_from_profile(
                profile.clone(),
                &path.join("files.log"),
                "pw",
            )
            .unwrap();
            crate::node::file_message_identity::IndexedFileMessageIdentities::from_profile(
                profile.clone(),
                &path.join("file-identities.log"),
                "pw",
            )
            .unwrap();
            (IndexedFileBook::open(profile).unwrap(), records)
        }
        fn fixture(&self, n: u8) -> (ReceivedEntry, Event, AnyManifest) {
            let manifest = crate::file::FileManifest {
                name: "retained.txt".into(),
                size: 3,
                mime: "text/plain".into(),
                checksum: [5; 32],
                file_key: [6; 32],
                file_conv: ConversationId::new([n; 32]),
                chunk_count: 1,
            };
            let original = Event::new(
                &self.device,
                ConversationId::new([250; 32]),
                1,
                vec![],
                1,
                42,
                EventKind::FileManifest,
                vec![n],
            );
            let entry = ReceivedEntry {
                event_id: original.id,
                conversation: original.conversation_id,
                from: self.device.user_id(),
                wall_clock: 42,
                plaintext: manifest.encode(),
            };
            (entry, original, AnyManifest::V1(manifest))
        }
        fn install(
            c: &Connection,
            entry: &ReceivedEntry,
            original: &Event,
        ) -> Result<bool, LogError> {
            TranscriptRepository::put_on(
                c,
                FILES,
                entry.conversation,
                entry.event_id.as_bytes(),
                entry.wall_clock,
                0,
                &entry.from,
                &entry.plaintext,
                false,
                false,
            )?;
            install_on(c, entry, original)
        }
    }

    #[test]
    fn file_binding_and_processed_evidence_commit_with_body_and_survive_reopen() {
        let f = Fixture::new();
        let (entry, original, manifest) = f.fixture(1);
        let aborted: Result<(), LogError> = f.book.profile.database().transaction(|c| {
            Fixture::install(c, &entry, &original)?;
            Err(LogError::CorruptFile(
                "injected later projection failure".into(),
            ))
        });
        assert!(aborted.is_err());
        assert!(f.records.entry(entry.event_id).unwrap().is_none());
        assert!(!f.book.processed(entry.event_id).unwrap());
        assert!(
            f.book
                .profile
                .database()
                .transaction(|c| Fixture::install(c, &entry, &original))
                .unwrap()
        );
        assert!(
            !f.book
                .profile
                .database()
                .transaction(|c| Fixture::install(c, &entry, &original))
                .unwrap()
        );
        assert_eq!(
            f.book.manifest(manifest.file_conv()).unwrap(),
            Some(manifest.clone())
        );
        assert!(f.book.processed(entry.event_id).unwrap());
        let Fixture {
            root,
            device,
            account,
            book,
            records,
        } = f;
        drop(records);
        drop(book);
        let (book, _records) = Fixture::open(root.path(), &device, &account);
        assert_eq!(book.manifest(manifest.file_conv()).unwrap(), Some(manifest));
        assert!(book.processed(entry.event_id).unwrap());
    }

    #[test]
    fn physical_alias_deletion_revokes_eligibility_but_keeps_exact_processing_evidence() {
        let mut f = Fixture::new();
        let (first, original, manifest) = f.fixture(2);
        let alias = Event::new(
            &f.device,
            original.conversation_id,
            2,
            vec![],
            2,
            42,
            EventKind::FileManifest,
            vec![99],
        );
        let mut second = first.clone();
        second.event_id = alias.id;
        f.book
            .profile
            .database()
            .transaction(|c| {
                Fixture::install(c, &first, &original)?;
                Fixture::install(c, &second, &alias)?;
                Ok(())
            })
            .unwrap();
        assert_eq!(f.book.revision().unwrap(), 2);
        let offers = f
            .book
            .offers_page(Some(manifest.file_conv()), None, None, 128, 1024)
            .unwrap();
        assert_eq!(offers.len(), 2);
        assert!(f.book.eligible_ordinal(offers[0].ordinal).unwrap());
        assert_eq!(
            f.book.files_page(None, 128, 1024).unwrap(),
            vec![manifest.file_conv()]
        );
        f.book
            .profile
            .database()
            .transaction(|c| Fixture::install(c, &first, &original))
            .unwrap();
        assert_eq!(f.book.revision().unwrap(), 2);
        f.records
            .remove_where(|row| row.event_id == first.event_id)
            .unwrap();
        assert_eq!(f.book.revision().unwrap(), 3);
        assert!(!f.book.eligible_ordinal(offers[0].ordinal).unwrap());
        assert_eq!(
            f.book.manifest(manifest.file_conv()).unwrap(),
            Some(manifest.clone())
        );
        f.records
            .remove_where(|row| row.event_id == second.event_id)
            .unwrap();
        assert_eq!(f.book.revision().unwrap(), 4);
        assert!(f.book.manifest(manifest.file_conv()).unwrap().is_none());
        assert!(f.book.files_page(None, 128, 1024).unwrap().is_empty());
        assert!(f.book.processed(first.event_id).unwrap());
        assert!(f.book.processed(second.event_id).unwrap());
    }

    #[test]
    fn durable_suppression_revokes_file_key_before_physical_row_cleanup() {
        let f = Fixture::new();
        let (entry, original, manifest) = f.fixture(5);
        f.book
            .profile
            .database()
            .transaction(|c| {
                Fixture::install(c, &entry, &original)?;
                Ok(())
            })
            .unwrap();
        let ordinal = f.book.offers_page(None, None, None, 1, 144).unwrap()[0].ordinal;
        f.book
            .profile
            .database()
            .transaction(|c| {
                c.execute(
                    "INSERT INTO transcript_suppression VALUES(?1,?2)",
                    params![
                        entry.conversation.as_bytes().as_slice(),
                        entry.event_id.as_bytes().as_slice()
                    ],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(f.records.entry(entry.event_id).unwrap().is_some());
        assert!(f.book.manifest(manifest.file_conv()).unwrap().is_none());
        assert!(!f.book.eligible_ordinal(ordinal).unwrap());
        assert_eq!(f.book.revision().unwrap(), 2);
        assert!(f.book.files_page(None, 1, 32).unwrap().is_empty());
        assert!(f.book.processed(entry.event_id).unwrap());
        assert!(
            !f.book
                .profile
                .database()
                .transaction(|c| Fixture::install(c, &entry, &original))
                .unwrap()
        );
        assert!(f.book.manifest(manifest.file_conv()).unwrap().is_none());
    }

    #[test]
    fn canonical_suppression_revokes_raw_aliases_in_either_arrival_order() {
        for identity_first in [false, true] {
            let f = Fixture::new();
            let (entry, original, manifest) = f.fixture(6);
            let host = ConversationId::new([210; 32]);
            let logical = EventId::new([211; 32]);
            // These are the indexed owner's authenticated identity facts. No
            // identity or transcript body is needed by the deletion projection.
            let bind = |c: &Connection| -> Result<(), LogError> {
                c.execute(
                    "INSERT INTO file_identity_headers VALUES(?1,?2,?3,?4,?5,?6,?7,?7,1)",
                    params![
                        host.as_bytes().as_slice(),
                        logical.as_bytes().as_slice(),
                        [212u8; 32].as_slice(),
                        original.author.ed25519_pub().as_slice(),
                        manifest.file_conv().as_bytes().as_slice(),
                        <[u8; 32]>::from(Sha256::digest(&entry.plaintext)).as_slice(),
                        1u64.to_be_bytes().as_slice()
                    ],
                )?;
                c.execute(
                    "INSERT INTO file_identity_aliases VALUES(?1,?2,?3)",
                    params![
                        entry.event_id.as_bytes().as_slice(),
                        host.as_bytes().as_slice(),
                        logical.as_bytes().as_slice()
                    ],
                )?;
                Ok(())
            };
            f.book
                .profile
                .database()
                .transaction(|c| {
                    Fixture::install(c, &entry, &original)?;
                    if identity_first {
                        bind(c)?;
                    }
                    c.execute(
                        "INSERT INTO transcript_suppression VALUES(?1,?2)",
                        params![host.as_bytes().as_slice(), logical.as_bytes().as_slice()],
                    )?;
                    if !identity_first {
                        bind(c)?;
                    }
                    Ok(())
                })
                .unwrap();
            assert!(f.records.entry(entry.event_id).unwrap().is_some());
            assert!(f.book.manifest(manifest.file_conv()).unwrap().is_none());
            assert_eq!(f.book.revision().unwrap(), 2);
            assert!(
                !f.book
                    .profile
                    .database()
                    .transaction(|c| Fixture::install(c, &entry, &original))
                    .unwrap()
            );
        }
    }

    #[test]
    fn file_metadata_pages_and_warm_open_never_fetch_unselected_manifest_bodies() {
        let f = Fixture::new();
        for n in 1..=140 {
            let (entry, original, _) = f.fixture(n);
            f.book
                .profile
                .database()
                .transaction(|c| Fixture::install(c, &entry, &original))
                .unwrap();
        }
        assert_eq!(f.book.files_page(None, 128, 64).unwrap().len(), 2);
        assert_eq!(
            f.book
                .offers_page(None, None, None, 128, 288)
                .unwrap()
                .len(),
            2
        );
        assert!(matches!(
            f.book.files_page(None, 129, 1024),
            Err(LogError::InvalidPageBudget)
        ));
        assert!(matches!(
            f.book.offers_page(None, None, None, 128, 143),
            Err(LogError::PageBudgetTooSmall { required: 144 })
        ));
        f.book.profile.database().transaction(|c| { c.execute("UPDATE transcript_bodies SET plaintext=zeroblob(?1) WHERE ordinal=(SELECT min(ordinal) FROM transcript_rows WHERE kind=3)",[MAX_BODY as i64+1])?; Ok(()) }).unwrap();
        let reopened = IndexedFileBook::open(f.book.profile.clone()).unwrap();
        assert_eq!(reopened.files_page(None, 128, 64).unwrap().len(), 2);
        assert!(
            matches!(reopened.manifest(ConversationId::new([1;32])),Err(LogError::CorruptFile(error)) if error.contains("allocation charge"))
        );
        assert!(
            reopened
                .manifest(ConversationId::new([140; 32]))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn account_adoption_fences_old_book_without_importing_former_account_offers() {
        let f = Fixture::new();
        let (entry, original, manifest) = f.fixture(3);
        f.book
            .profile
            .database()
            .transaction(|c| Fixture::install(c, &entry, &original))
            .unwrap();
        let ordinal = f.book.offers_page(None, None, None, 1, 144).unwrap()[0].ordinal;
        let adopted = Account::generate();
        f.book
            .profile
            .database()
            .transaction(|c| {
                c.execute(
                    "UPDATE participant_profile SET account=?1 WHERE singleton=1",
                    [adopted.public().ed25519_pub.as_slice()],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(matches!(
            f.book.eligible_ordinal(ordinal),
            Err(LogError::StaleCursor)
        ));
        assert!(matches!(f.book.revision(), Err(LogError::StaleCursor)));
        let next = IndexedFileBook::open(f.book.profile.clone()).unwrap();
        assert_eq!(next.import_position().unwrap(), None);
        assert_eq!(next.revision().unwrap(), 0);
        assert!(next.manifest(manifest.file_conv()).unwrap().is_none());
        assert!(!next.processed(entry.event_id).unwrap());
    }

    #[test]
    fn migration_checkpoint_rolls_back_with_file_binding_and_requires_exact_prefix() {
        let f = Fixture::new();
        assert_eq!(f.book.import_position().unwrap(), Some(None));
        let (entry, original, _) = f.fixture(4);
        let aborted: Result<(), LogError> = f.book.profile.database().transaction(|c| {
            Fixture::install(c, &entry, &original)?;
            f.book.advance_import_on(c, None, Some(1))?;
            Err(invalid("later failure"))
        });
        assert!(aborted.is_err());
        assert_eq!(f.book.import_position().unwrap(), Some(None));
        assert!(!f.book.processed(entry.event_id).unwrap());
        f.book
            .profile
            .database()
            .transaction(|c| f.book.advance_import_on(c, None, Some(1)))
            .unwrap();
        assert_eq!(f.book.import_position().unwrap(), Some(Some(1)));
        assert!(matches!(
            f.book
                .profile
                .database()
                .transaction(|c| f.book.advance_import_on(c, None, Some(2))),
            Err(LogError::StaleCursor)
        ));
        f.book
            .profile
            .database()
            .transaction(|c| f.book.advance_import_on(c, Some(1), None))
            .unwrap();
        assert_eq!(f.book.import_position().unwrap(), None);
    }
    #[test]
    fn file_receive_projection_installs_durable_body_binding_and_first_receipt() {
        let mut f = Fixture::new();
        let effects =
            crate::node::reception_effects::ReceptionEffects::open(f.book.profile.clone()).unwrap();
        let after = effects.head().unwrap();
        let (entry, original, manifest) = f.fixture(7);
        let mut metadata = crate::node::reception_effects::ReceptionMetadata::for_incoming(
            &original,
            &entry,
            &f.device.public(),
            &f.account.account_id(),
            1234,
        );
        f.records
            .record_file_with_reception(&entry, Some(&original), Some(&metadata))
            .unwrap();
        assert_eq!(
            f.book.manifest(manifest.file_conv()).unwrap(),
            Some(manifest)
        );
        assert!(f.book.processed(entry.event_id).unwrap());
        let head = effects.head().unwrap();
        assert_ne!(head, after);
        metadata.received_at = 9999;
        f.records
            .record_file_with_reception(&entry, Some(&original), Some(&metadata))
            .unwrap();
        assert_eq!(effects.head().unwrap(), head);
        let page = effects.page(after, head, Default::default(), 1234).unwrap();
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].metadata.received_at, 1234);
        assert_eq!(
            page.rows[0].source.kind,
            crate::node::reception_effects::ReceptionKind::File
        );
    }

    #[test]
    fn receipt_storage_failure_rolls_back_file_body_binding_and_processing_evidence() {
        let mut f = Fixture::new();
        let effects =
            crate::node::reception_effects::ReceptionEffects::open(f.book.profile.clone()).unwrap();
        let after = effects.head().unwrap();
        let (entry, original, manifest) = f.fixture(8);
        let metadata = crate::node::reception_effects::ReceptionMetadata::for_incoming(
            &original,
            &entry,
            &f.device.public(),
            &f.account.account_id(),
            1234,
        );
        f.book.profile.database().transaction(|c| {
            c.execute_batch("CREATE TRIGGER reject_file_reception BEFORE INSERT ON reception_pending BEGIN SELECT RAISE(ABORT,'injected reception write failure'); END;")?;
            Ok(())
        }).unwrap();
        assert!(
            f.records
                .record_file_with_reception(&entry, Some(&original), Some(&metadata))
                .is_err()
        );
        assert!(f.records.entry(entry.event_id).unwrap().is_none());
        assert!(f.book.manifest(manifest.file_conv()).unwrap().is_none());
        assert!(!f.book.processed(entry.event_id).unwrap());
        assert_eq!(effects.head().unwrap(), after);
        assert_eq!(f.book.revision().unwrap(), 0);
        f.book
            .profile
            .database()
            .transaction(|c| {
                c.execute_batch("DROP TRIGGER reject_file_reception;")?;
                Ok(())
            })
            .unwrap();
        f.records
            .record_file_with_reception(&entry, Some(&original), Some(&metadata))
            .unwrap();
        assert_eq!(
            f.book.manifest(manifest.file_conv()).unwrap(),
            Some(manifest)
        );
        assert_ne!(effects.head().unwrap(), after);
    }
    #[test]
    fn display_aliases_keep_the_established_file_transfer_binding() {
        let mut f = Fixture::new();
        let (first, original, manifest) = f.fixture(9);
        let AnyManifest::V1(mut renamed) = manifest.clone() else {
            unreachable!()
        };
        renamed.name = "renamed.txt".into();
        renamed.mime = "application/octet-stream".into();
        let second_original = Event::new(
            &f.device,
            original.conversation_id,
            2,
            vec![],
            2,
            42,
            EventKind::FileManifest,
            vec![222],
        );
        let mut second = first.clone();
        second.event_id = second_original.id;
        second.plaintext = renamed.encode();
        assert!(crate::node::files::same_file_transfer(
            &manifest,
            &AnyManifest::V1(renamed.clone())
        ));
        f.book
            .profile
            .database()
            .transaction(|c| {
                Fixture::install(c, &first, &original)?;
                Fixture::install(c, &second, &second_original)?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            f.book.manifest(manifest.file_conv()).unwrap(),
            Some(manifest.clone())
        );
        f.records
            .remove_where(|row| row.event_id == first.event_id)
            .unwrap();
        assert_eq!(
            f.book.manifest(manifest.file_conv()).unwrap(),
            Some(AnyManifest::V1(renamed))
        );
    }

    #[test]
    fn retired_account_producers_cannot_install_effects_in_the_adopted_namespace() {
        let mut f = Fixture::new();
        let (entry, original, _) = f.fixture(10);
        let effects =
            crate::node::reception_effects::ReceptionEffects::open(f.book.profile.clone()).unwrap();
        let mut messages = ReceivedStore::from_profile(
            f.book.profile.clone(),
            &f.root.path().join("received.log"),
            "pw",
        )
        .unwrap();
        let metadata = crate::node::reception_effects::ReceptionMetadata::for_incoming(
            &original,
            &entry,
            &f.device.public(),
            &f.account.account_id(),
            1234,
        );
        let adopted = Account::generate();
        f.book
            .profile
            .database()
            .transaction(|c| {
                c.execute(
                    "UPDATE participant_profile SET account=?1 WHERE singleton=1",
                    [adopted.public().ed25519_pub.as_slice()],
                )?;
                Ok(())
            })
            .unwrap();
        let next =
            crate::node::reception_effects::ReceptionEffects::open(f.book.profile.clone()).unwrap();
        let next_head = next.head().unwrap();
        assert!(matches!(
            f.records
                .record_file_with_reception(&entry, Some(&original), Some(&metadata)),
            Err(LogError::StaleCursor)
        ));
        assert!(matches!(
            messages.record_durable_with_reception(&entry, &metadata),
            Err(LogError::StaleCursor)
        ));
        assert!(matches!(effects.head(), Err(LogError::StaleCursor)));
        assert_eq!(next.head().unwrap(), next_head);
        assert!(f.records.entry(entry.event_id).unwrap().is_none());
        assert!(messages.entry(entry.event_id).unwrap().is_none());
    }
}
