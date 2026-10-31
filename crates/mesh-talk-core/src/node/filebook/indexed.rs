//! Account-scoped file bindings and exact processing evidence on the participant
//! connection. Payloads stay in the existing FILES transcript source.
use crate::{
    eventlog::{ConversationId, Event, EventId, LogError},
    file::AnyManifest,
    node::received_log::ReceivedEntry,
    storage::profile::{
        context::ProfileStorage,
        participant::event_ownership::{self, ParticipantEventOwner},
    },
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
CREATE INDEX file_book_file_authors ON file_book_offers(account,file,author,event);
CREATE INDEX file_book_author_offers ON file_book_offers(account,author,ordinal);
CREATE INDEX file_book_event_offers ON file_book_offers(account,event,ordinal);
CREATE TABLE file_book_reconcile(account BLOB NOT NULL CHECK(length(account)=32),ordinal INTEGER NOT NULL,PRIMARY KEY(account,ordinal)) WITHOUT ROWID;
CREATE TABLE file_book_processed(account BLOB NOT NULL CHECK(length(account)=32), event BLOB NOT NULL CHECK(length(event)=32), PRIMARY KEY(account,event)) WITHOUT ROWID;
";
const TRIGGERS: &[(&str, &str)] = &[
    (
        "file_book_offer_added",
        "CREATE TRIGGER file_book_offer_added AFTER INSERT ON file_book_offers BEGIN UPDATE file_book_bindings SET offers=offers+1 WHERE account=NEW.account AND file=NEW.file; UPDATE file_book_owners SET revision=revision+1 WHERE account=NEW.account; END",
    ),
    (
        "file_book_offer_removed",
        "CREATE TRIGGER file_book_offer_removed AFTER DELETE ON file_book_offers BEGIN UPDATE file_book_bindings SET offers=offers-1 WHERE account=OLD.account AND file=OLD.file; UPDATE file_book_owners SET revision=revision+1 WHERE account=OLD.account; DELETE FROM file_download_pending WHERE account=OLD.account AND file=OLD.file AND EXISTS(SELECT 1 FROM file_book_bindings WHERE account=OLD.account AND file=OLD.file AND offers=0); END",
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
pub(super) fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}
pub(super) fn blob<const N: usize>(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> Result<[u8; N], LogError> {
    row.get_ref(index)?
        .as_blob()
        .map_err(|_| invalid("invalid file-book metadata"))?
        .try_into()
        .map_err(|_| invalid("invalid file-book metadata width"))
}
pub(super) fn active(c: &Connection) -> Result<[u8; 32], LogError> {
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

#[derive(Clone)]
pub(crate) struct IndexedFileBook {
    pub(super) profile: Arc<ProfileStorage>,
    pub(super) account: [u8; 32],
    // Early WAL/migration setup may precede adoption publication. Only the
    // recaptured final owner may schedule newly received events.
    pub(super) receive_owner: Option<ParticipantEventOwner>,
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
    /// Metadata-only traversal. Each turn releases the profile connection before
    /// the caller processes a row; no payload or history-sized ID list is kept.
    pub(crate) fn offers(
        &self,
        file: Option<ConversationId>,
        author: Option<crate::eventlog::Author>,
    ) -> impl Iterator<Item = Result<FileOffer, LogError>> + use<> {
        let book = self.clone();
        let mut after = None;
        let mut pending = std::collections::VecDeque::new();
        let mut done = false;
        std::iter::from_fn(move || {
            if pending.is_empty() && !done {
                match book.offers_page(file, author, after, 128, 128 * 144) {
                    Ok(page) => {
                        done = page.len() < 128;
                        after = page.last().map(|offer| offer.ordinal).or(after);
                        pending.extend(page);
                    }
                    Err(error) => {
                        done = true;
                        return Some(Err(error));
                    }
                }
            }
            pending.pop_front().map(Ok)
        })
    }
    pub(crate) fn files(&self) -> impl Iterator<Item = Result<ConversationId, LogError>> + use<> {
        let book = self.clone();
        let mut after = None;
        let mut pending = std::collections::VecDeque::new();
        let mut done = false;
        std::iter::from_fn(move || {
            if pending.is_empty() && !done {
                match book.files_page(after, 128, 4096) {
                    Ok(page) => {
                        done = page.len() < 128;
                        after = page.last().copied().or(after);
                        pending.extend(page);
                    }
                    Err(error) => {
                        done = true;
                        return Some(Err(error));
                    }
                }
            }
            pending.pop_front().map(Ok)
        })
    }
    pub(crate) fn begin_reconcile(&self) -> Result<u64, LogError> {
        self.profile.database().transaction(|c| {
            self.check(c)?;
            c.execute(
                "DELETE FROM file_book_reconcile WHERE account=?1",
                [self.account.as_slice()],
            )?;
            Ok(())
        })?;
        self.revision()
    }
    pub(crate) fn stage_revoke(&self, ordinal: i64) -> Result<(), LogError> {
        self.profile.database().transaction(|c| {
            self.check(c)?;
            c.execute(
                "INSERT OR IGNORE INTO file_book_reconcile VALUES(?1,?2)",
                params![self.account.as_slice(), ordinal],
            )?;
            Ok(())
        })
    }
    pub(crate) fn finish_reconcile(&self, revision: u64) -> Result<(), LogError> {
        self.profile.database().transaction(|c| {
            self.check(c)?;
            let current:i64=c.query_row("SELECT revision FROM file_book_owners WHERE account=?1",[self.account.as_slice()],|r|r.get(0))?;
            if u64::try_from(current).ok()!=Some(revision) {return Err(LogError::StaleCursor);}
            c.execute("DELETE FROM file_book_offers WHERE account=?1 AND ordinal IN(SELECT ordinal FROM file_book_reconcile WHERE account=?1)",[self.account.as_slice()])?;
            c.execute("DELETE FROM file_book_reconcile WHERE account=?1",[self.account.as_slice()])?;
            Ok(())
        })
    }
    pub(crate) fn contains_file(&self, file: ConversationId) -> Result<bool, LogError> {
        self.profile.database().read(|c| {self.check(c)?;Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM file_book_bindings WHERE account=?1 AND file=?2 AND offers>0)",params![self.account.as_slice(),file.as_bytes().as_slice()],|r|r.get(0))?)})
    }
    pub(crate) fn profile(&self) -> &Arc<ProfileStorage> {
        &self.profile
    }
    pub(crate) fn accepts(&self, manifest: &AnyManifest) -> Result<bool, LogError> {
        let binding = crate::node::files::file_transfer_fingerprint(manifest)
            .ok_or_else(|| invalid("invalid file transfer binding"))?;
        self.profile.database().read(|c| {
            self.check(c)?;
            let stored: Option<Vec<u8>> = c
                .query_row(
                    "SELECT digest FROM file_book_bindings WHERE account=?1 AND file=?2",
                    params![
                        self.account.as_slice(),
                        manifest.file_conv().as_bytes().as_slice()
                    ],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(stored.as_deref().is_none_or(|stored| stored == binding))
        })
    }
    pub(crate) fn has_author(
        &self,
        file: ConversationId,
        author: crate::eventlog::Author,
    ) -> Result<bool, LogError> {
        self.profile.database().read(|c| {
            self.check(c)?;
            Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM file_book_offers WHERE account=?1 AND file=?2 AND author=?3)",params![self.account.as_slice(),file.as_bytes().as_slice(),author.ed25519_pub().as_slice()],|r|r.get(0))?)
        })
    }
    pub(crate) fn has_manifest_event(
        &self,
        file: ConversationId,
        event: EventId,
    ) -> Result<bool, LogError> {
        self.profile.database().read(|c| {
            self.check(c)?;
            Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM file_book_offers WHERE account=?1 AND event=?2 AND file=?3)",params![self.account.as_slice(),event.as_bytes().as_slice(),file.as_bytes().as_slice()],|r|r.get(0))?)
        })
    }
    pub(crate) fn mark_processed(&self, event: EventId) -> Result<(), LogError> {
        self.profile.database().transaction(|c| {
            self.check(c)?;
            mark_processed_on(c, event)
        })
    }
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
    pub(crate) fn offer_for_entry(
        &self,
        entry: &ReceivedEntry,
    ) -> Result<Option<FileOffer>, LogError> {
        let digest: [u8; 32] = Sha256::digest(&entry.plaintext).into();
        self.profile.database().read(|c| {
            self.check(c)?;
            let mut statement = c.prepare_cached("SELECT o.ordinal,o.event,o.file,o.parent,o.author FROM file_book_offers o JOIN transcript_rows r ON r.ordinal=o.ordinal WHERE o.account=?1 AND r.kind=3 AND r.conversation=?2 AND r.row_key=?3 AND r.plaintext_sha256=?4 LIMIT 1")?;
            let mut rows=statement.query(params![self.account.as_slice(),entry.conversation.as_bytes().as_slice(),entry.event_id.as_bytes().as_slice(),digest.as_slice()])?;
            rows.next()?.map(|row| Ok(FileOffer { ordinal:row.get(0)?,event:EventId::new(blob(row,1)?),file:ConversationId::new(blob(row,2)?),parent:ConversationId::new(blob(row,3)?),author:crate::eventlog::Author::from_ed25519(blob(row,4)?) })).transpose()
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
        let (account, receive_owner) = profile.database().transaction(|c| {
            let present: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='file_book_owners')",
                [],
                |r| r.get(0),
            )?;
            if !present {
                c.execute_batch(SCHEMA)?;
                c.execute_batch(super::work::SCHEMA)?;
                for (_, sql) in TRIGGERS {
                    c.execute_batch(sql)?;
                }
            }
            crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
            crate::storage::profile::manifest::validate_ddl(c, super::work::SCHEMA)?;
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
            super::historical::initialize_on(c)?;
            super::retention::initialize_on(c)?;
            super::receiving::initialize_on(c, &account)?;
            // Only the initial account may import the old unnamespaced source.
            // A newly adopted account starts with no offers.
            c.execute(
                "INSERT OR IGNORE INTO file_book_owners VALUES(?1,NULL,?2,0)",
                params![account.as_slice(), i64::from(present)],
            )?;
            c.execute(
                "INSERT OR IGNORE INTO file_download_scan VALUES(?1,NULL,NULL,0,1)",
                [account.as_slice()],
            )?;
            let loaded = crate::identity::account::AccountPublic {
                ed25519_pub: account,
            };
            let receive_owner = match event_ownership::capture_event_owner_on(c, &loaded) {
                Ok(owner) => Some(owner),
                Err(LogError::StaleCursor) => None,
                Err(error) => return Err(error),
            };
            Ok((account, receive_owner))
        })?;
        Ok(Self {
            profile,
            account,
            receive_owner,
        })
    }
    pub(super) fn check(&self, c: &Connection) -> Result<(), LogError> {
        if let Some(owner) = &self.receive_owner {
            return event_ownership::check_event_owner_on(c, owner);
        }
        // A temporary migration handle becomes stale once adoption publishes,
        // even when the final account happens to have the same public key.
        let migration: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM participant_profile WHERE singleton=1 AND account=?1 AND bootstrap_previous IS NOT NULL)", [self.account.as_slice()], |r|r.get(0))?;
        if migration {
            Ok(())
        } else {
            Err(LogError::StaleCursor)
        }
    }
    pub(crate) fn manifest(
        &self,
        file: impl std::borrow::Borrow<ConversationId>,
    ) -> Result<Option<AnyManifest>, LogError> {
        let file = *file.borrow();
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
    c.execute(
        "DELETE FROM file_receive_pending WHERE account=?1 AND event=?2",
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
    super::retention::record_on(c, &account, ordinal, entry.wall_clock, &manifest)?;
    if fresh {
        super::work::enqueue_on(c, manifest.file_conv())?;
    }
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
            drop(
                crate::eventlog::persist::PersistentEventLog::open_profile(profile.clone(), "pw")
                    .unwrap(),
            );
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
    fn file_retention_current_order_survives_upgrade_and_rolls_back_erasure() {
        let f = Fixture::new();
        let (first, original, manifest) = f.fixture(71);
        let alias = Event::new(
            &f.device,
            original.conversation_id,
            2,
            vec![],
            2,
            u64::MAX,
            EventKind::FileManifest,
            vec![99],
        );
        let mut latest = first.clone();
        latest.event_id = alias.id;
        latest.wall_clock = u64::MAX;
        let profile = f.book.profile.clone();
        profile.database().transaction(|c| {
            Fixture::install(c, &first, &original)?;
            Fixture::install(c, &latest, &alias)?;
            c.execute_batch("DROP TRIGGER file_book_retention_added; DROP TRIGGER file_book_retention_removed; DROP TABLE file_book_retention; DROP TABLE file_book_offer_retention;")?;
            Ok(())
        }).unwrap();
        let upgraded = IndexedFileBook::open(profile.clone()).unwrap();
        let page = upgraded.retention_page(None).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(
            (page[0].file, page[0].newest, page[0].chunks),
            (manifest.file_conv(), u64::MAX, 1)
        );
        assert!(
            upgraded
                .retention_page(Some((u64::MAX, manifest.file_conv())))
                .unwrap()
                .is_empty()
        );
        let failed: Result<(), LogError> = profile.database().transaction(|c| {
            c.execute(
                "DELETE FROM transcript_rows WHERE row_key=?1",
                [latest.event_id.as_bytes().as_slice()],
            )?;
            Err(invalid("injected later retirement failure"))
        });
        assert!(failed.is_err());
        assert_eq!(
            upgraded
                .retention_for(manifest.file_conv())
                .unwrap()
                .unwrap()
                .newest,
            u64::MAX
        );
        profile
            .database()
            .transaction(|c| {
                c.execute(
                    "DELETE FROM transcript_rows WHERE row_key=?1",
                    [latest.event_id.as_bytes().as_slice()],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            upgraded
                .retention_for(manifest.file_conv())
                .unwrap()
                .unwrap()
                .newest,
            42
        );
        // A warm open uses only projected metadata. A corrupt selected body is
        // still rejected by the body owner, rather than decoded during startup.
        profile.database().transaction(|c| {
            c.execute("UPDATE transcript_bodies SET plaintext=zeroblob(length(plaintext)) WHERE ordinal IN (SELECT ordinal FROM transcript_rows WHERE row_key=?1)", [first.event_id.as_bytes().as_slice()])?;
            Ok(())
        }).unwrap();
        let warm = IndexedFileBook::open(profile.clone()).unwrap();
        assert_eq!(warm.retention_page(None).unwrap().len(), 1);
        assert!(warm.manifest(manifest.file_conv()).is_err());
        profile
            .database()
            .transaction(|c| {
                c.execute(
                    "DELETE FROM transcript_rows WHERE row_key=?1",
                    [first.event_id.as_bytes().as_slice()],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(warm.retention_page(None).unwrap().is_empty());
    }

    #[test]
    fn file_retention_upgrade_rejects_changed_binding_atomically() {
        let f = Fixture::new();
        let (entry, original, _) = f.fixture(72);
        let profile = f.book.profile.clone();
        profile.database().transaction(|c| {
            Fixture::install(c, &entry, &original)?;
            c.execute_batch("DROP TRIGGER file_book_retention_added; DROP TRIGGER file_book_retention_removed; DROP TABLE file_book_retention; DROP TABLE file_book_offer_retention; UPDATE file_book_bindings SET digest=zeroblob(32);")?;
            Ok(())
        }).unwrap();
        assert!(IndexedFileBook::open(profile.clone()).is_err());
        let published: bool = profile
            .database()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='file_book_retention')",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert!(
            !published,
            "failed derived migration must roll back its schema"
        );
        assert!(
            f.records.entry(entry.event_id).unwrap().is_some(),
            "authoritative body survives a failed derived migration"
        );
    }

    #[test]
    fn historical_file_eligibility_survives_adoption_without_current_offer_authority() {
        use crate::storage::profile::participant;
        let f = Fixture::new();
        let (entry, original, manifest) = f.fixture(61);
        let profile = f.book.profile.clone();
        profile
            .database()
            .transaction(|c| Fixture::install(c, &entry, &original))
            .unwrap();
        let accepted = f.book.historical_offers_after(0, 1).unwrap();
        assert_eq!(accepted.len(), 1);
        let ordinal = accepted[0].ordinal;
        assert_eq!(accepted[0].account, f.account.public().ed25519_pub);
        assert_eq!(f.book.historical_revisions().unwrap(), (1, 0));
        let adopted = Account::generate();
        participant::prepare_adoption(profile.database(), &f.account.public(), &adopted.public())
            .unwrap();
        participant::finish_adoption(profile.database(), &adopted.public()).unwrap();
        let current = IndexedFileBook::open(profile.clone()).unwrap();
        assert!(!current.eligible_ordinal(ordinal).unwrap());
        assert!(current.manifest(manifest.file_conv()).unwrap().is_none());
        assert!(current.historical_eligible_ordinal(ordinal).unwrap());
        assert!(matches!(
            f.book.historical_revision(),
            Err(LogError::StaleCursor)
        ));
        let failed: Result<(), LogError> = profile.database().transaction(|c| {
            c.execute("DELETE FROM transcript_rows WHERE ordinal=?1", [ordinal])?;
            Err(invalid("injected file retirement failure"))
        });
        assert!(failed.is_err());
        assert!(current.historical_eligible_ordinal(ordinal).unwrap());
        assert_eq!(current.historical_revisions().unwrap(), (1, 0));
        profile
            .database()
            .transaction(|c| {
                c.execute("DELETE FROM transcript_rows WHERE ordinal=?1", [ordinal])?;
                Ok(())
            })
            .unwrap();
        assert!(!current.historical_eligible_ordinal(ordinal).unwrap());
        assert_eq!(current.historical_revisions().unwrap(), (2, 2));
        assert!(current.historical_offers_after(0, 128).unwrap().is_empty());
    }

    #[test]
    fn historical_file_metadata_upgrade_keeps_existing_offers_as_initial_snapshot() {
        let f = Fixture::new();
        let (entry, original, _) = f.fixture(62);
        let profile = f.book.profile.clone();
        profile.database().transaction(|c|{
            Fixture::install(c, &entry, &original)?;
            c.execute_batch("DROP TRIGGER file_book_history_added; DROP TRIGGER file_book_history_removed; DROP TABLE file_book_offer_revisions; DROP TABLE file_book_history_state;")?;
            Ok(())
        }).unwrap();
        let upgraded = IndexedFileBook::open(profile.clone()).unwrap();
        let ordinal: i64 = profile
            .database()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT ordinal FROM transcript_rows WHERE kind=3 AND row_key=?1",
                    [entry.event_id.as_bytes().as_slice()],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert!(upgraded.historical_eligible_ordinal(ordinal).unwrap());
        assert_eq!(upgraded.historical_revisions().unwrap(), (0, 0));
        assert!(upgraded.historical_offers_after(0, 128).unwrap().is_empty());
        let (next, event, _) = f.fixture(63);
        profile
            .database()
            .transaction(|c| Fixture::install(c, &next, &event))
            .unwrap();
        let page = upgraded.historical_offers_after(0, 1).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].revision, 1);
        assert_ne!(page[0].ordinal, ordinal);
        assert!(upgraded.historical_offers_after(1, 1).unwrap().is_empty());
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
        let mut log = crate::eventlog::persist::PersistentEventLog::open_profile(
            f.book.profile.clone(),
            "pw",
        )
        .unwrap();
        log.append_durable(original.clone()).unwrap();
        f.book
            .profile
            .database()
            .transaction(|c| Fixture::install(c, &entry, &original))
            .unwrap();
        let ordinal = f.book.offers_page(None, None, None, 1, 144).unwrap()[0].ordinal;
        let adopted = Account::generate();
        crate::storage::profile::participant::prepare_adoption(
            f.book.profile.database(),
            &f.account.public(),
            &adopted.public(),
        )
        .unwrap();
        crate::storage::profile::participant::finish_adoption(
            f.book.profile.database(),
            &adopted.public(),
        )
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
        crate::storage::profile::participant::prepare_adoption(
            f.book.profile.database(),
            &f.account.public(),
            &adopted.public(),
        )
        .unwrap();
        crate::storage::profile::participant::finish_adoption(
            f.book.profile.database(),
            &adopted.public(),
        )
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
    #[test]
    fn returned_account_producer_rejects_old_file_store_generation() {
        returned_account_producer(true);
    }

    #[test]
    fn returned_account_producer_rejects_old_message_store_generation() {
        returned_account_producer(false);
    }

    fn returned_account_producer(files: bool) {
        use crate::storage::profile::participant;
        let f = Fixture::new();
        let (mut entry, original, _) = f.fixture(27);
        if !files {
            entry.plaintext =
                crate::node::message::MessageBody::new(b"retained message".to_vec(), None).encode();
        }
        let path = f
            .root
            .path()
            .join(if files { "files.log" } else { "received.log" });
        let open = || {
            if files {
                ReceivedStore::file_cards_from_profile(f.book.profile.clone(), &path, "pw")
            } else {
                ReceivedStore::from_profile(f.book.profile.clone(), &path, "pw")
            }
        };
        let mut old = open().unwrap();
        let effects =
            crate::node::reception_effects::ReceptionEffects::open(f.book.profile.clone()).unwrap();
        let before = effects.head().unwrap();
        let metadata = crate::node::reception_effects::ReceptionMetadata::for_incoming(
            &original,
            &entry,
            &f.device.public(),
            &f.account.account_id(),
            1234,
        );
        let other = Account::generate();
        participant::prepare_adoption(
            f.book.profile.database(),
            &f.account.public(),
            &other.public(),
        )
        .unwrap();
        participant::finish_adoption(f.book.profile.database(), &other.public()).unwrap();
        participant::prepare_adoption(
            f.book.profile.database(),
            &other.public(),
            &f.account.public(),
        )
        .unwrap();
        participant::finish_adoption(f.book.profile.database(), &f.account.public()).unwrap();
        assert!(matches!(effects.head(), Err(LogError::StaleCursor)));
        let effects =
            crate::node::reception_effects::ReceptionEffects::open(f.book.profile.clone()).unwrap();
        let write = |store: &mut ReceivedStore| {
            if files {
                store.record_file_with_reception(&entry, Some(&original), Some(&metadata))
            } else {
                store.record_durable_with_reception(&entry, &metadata)
            }
        };
        assert!(
            matches!(write(&mut old), Err(LogError::StaleCursor)),
            "an old producer must not revive after its account returns"
        );
        assert!(old.entry(entry.event_id).unwrap().is_none());
        assert_eq!(
            effects.head().unwrap(),
            before,
            "stale writes must install no reception work"
        );
        let mut fresh = open().unwrap();
        write(&mut fresh).unwrap();
        assert!(fresh.entry(entry.event_id).unwrap().is_some());
        assert_ne!(effects.head().unwrap(), before);
    }

    #[test]
    fn durable_download_work_is_bounded_fair_and_survives_reopen() {
        let f = Fixture::new();
        for n in 1..=130 {
            let (row, event, _) = f.fixture(n);
            f.book
                .profile
                .database()
                .transaction(|c| Fixture::install(c, &row, &event))
                .unwrap();
        }
        f.book.discover_downloads(7).unwrap();
        f.book.discover_downloads(7).unwrap();
        let first = f.book.claim_downloads().unwrap();
        assert_eq!(first.len(), 8);
        let Fixture {
            root,
            device,
            account,
            book,
            records,
        } = f;
        drop(records);
        drop(book);
        let (book, records) = Fixture::open(root.path(), &device, &account);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..17 {
            let page = book.claim_downloads().unwrap();
            assert!(page.len() <= 8);
            for work in page {
                seen.insert(work.file);
                book.finish_download(work).unwrap();
            }
        }
        assert_eq!(seen.len(), 130);
        assert!(!book.download_work_pending().unwrap());
        assert_eq!(
            records.body_reads(),
            0,
            "download discovery and retries are metadata only"
        );
        book.discover_downloads(8).unwrap();
        assert_eq!(
            book.claim_downloads().unwrap().len(),
            8,
            "changed consent rechecks retained offers"
        );
    }
    #[test]
    fn download_failure_rolls_back_body_and_stale_ack_preserves_new_offer() {
        let f = Fixture::new();
        let (first, original, manifest) = f.fixture(1);
        f.book.profile.database().transaction(|c| {c.execute_batch("CREATE TRIGGER fail_download BEFORE INSERT ON file_download_pending BEGIN SELECT RAISE(ABORT,'injected download scheduling failure'); END")?;Ok(())}).unwrap();
        assert!(
            f.book
                .profile
                .database()
                .transaction(|c| Fixture::install(c, &first, &original))
                .is_err()
        );
        assert!(!f.book.processed(first.event_id).unwrap());
        assert!(f.records.entry(first.event_id).unwrap().is_none());
        f.book
            .profile
            .database()
            .transaction(|c| {
                c.execute_batch("DROP TRIGGER fail_download")?;
                Fixture::install(c, &first, &original)
            })
            .unwrap();
        let work = f.book.claim_downloads().unwrap()[0];
        let alias = Event::new(
            &f.device,
            original.conversation_id,
            2,
            vec![],
            2,
            42,
            EventKind::FileManifest,
            vec![9],
        );
        let mut second = first.clone();
        second.event_id = alias.id;
        f.book
            .profile
            .database()
            .transaction(|c| Fixture::install(c, &second, &alias))
            .unwrap();
        f.book.finish_download(work).unwrap();
        assert!(
            f.book.pending_for_test(manifest.file_conv()),
            "new durable offer cannot lose its only scheduling fact to an old completion"
        );
        let current = f.book.claim_downloads().unwrap()[0];
        assert!(current.revision > work.revision);
        f.book.finish_download(current).unwrap();
        assert!(!f.book.pending_for_test(manifest.file_conv()));
    }

    #[test]
    fn file_receive_discovery_is_atomic_fair_and_does_not_discard_large_legal_rows() {
        use crate::eventlog::{Author, persist::PersistentEventLog};
        let f = Fixture::new();
        let mut log = PersistentEventLog::open_profile(f.book.profile.clone(), "pw").unwrap();
        let sender = DeviceIdentity::generate();
        let own = Author::from_ed25519(f.device.public().ed25519_pub);
        let conversation = ConversationId::new([12; 32]);
        for sequence in 1..=40 {
            let event = Event::new(
                &sender,
                conversation,
                sequence,
                vec![],
                sequence,
                0,
                EventKind::FileManifest,
                vec![1; if sequence == 1 { 512 * 1024 } else { 100 }],
            );
            log.append_durable(event).unwrap();
        }
        let rows = log
            .repository()
            .unwrap()
            .committed_headers_page(None, 128)
            .unwrap();
        f.book.profile.database().transaction(|c| {
            c.execute_batch("CREATE TRIGGER fail_file_discovery BEFORE INSERT ON file_receive_pending WHEN NEW.ticket=20 BEGIN SELECT RAISE(ABORT,'injected'); END;")?;Ok(())
        }).unwrap();
        assert!(f.book.discover_receives(None, &rows, own).is_err());
        assert_eq!(f.book.receive_after().unwrap(), None);
        assert!(
            f.book
                .receive_candidates(conversation, 8, 4096)
                .unwrap()
                .is_empty()
        );
        f.book
            .profile
            .database()
            .transaction(|c| {
                c.execute_batch("DROP TRIGGER fail_file_discovery")?;
                Ok(())
            })
            .unwrap();
        f.book.discover_receives(None, &rows, own).unwrap();
        let first = f.book.receive_candidates(conversation, 8, 4096).unwrap();
        assert!(!first.contains(&rows[0].header.id));
        for id in &first {
            f.book.mark_processed(*id).unwrap();
        }
        let reopened = IndexedFileBook::open(f.book.profile.clone()).unwrap();
        assert_eq!(
            reopened.receive_after().unwrap(),
            rows.last().map(|row| row.ordinal)
        );
        let mut seen = std::collections::HashSet::new();
        for _ in 0..6 {
            seen.extend(
                reopened
                    .receive_candidates(conversation, 8, 8 * 1024 * 1024)
                    .unwrap(),
            );
        }
        assert!(
            seen.contains(&rows[0].header.id),
            "small budgets must not retire a valid large row"
        );
        assert_eq!(seen.len(), 40 - first.len());
        assert!(first.iter().all(|id| !seen.contains(id)));
        assert_eq!(f.records.body_reads(), 0);
    }

    #[test]
    fn file_receive_undiscovered_headers_keep_bounded_worker_scheduled() {
        use crate::eventlog::{Author, persist::PersistentEventLog};
        let f = Fixture::new();
        let mut log = PersistentEventLog::open_profile(f.book.profile.clone(), "pw").unwrap();
        f.book.discover_downloads(0).unwrap();
        assert!(!f.book.download_work_pending().unwrap());
        let sender = DeviceIdentity::generate();
        let conversation = ConversationId::new([23; 32]);
        for sequence in 1..=129 {
            log.append_durable(Event::new(
                &sender,
                conversation,
                sequence,
                vec![],
                sequence,
                0,
                EventKind::Message,
                vec![1],
            ))
            .unwrap();
        }
        assert!(
            f.book.download_work_pending().unwrap(),
            "an undiscovered header prefix must keep the worker at its work cadence"
        );
        let own = Author::from_ed25519(f.device.public().ed25519_pub);
        let first = log
            .repository()
            .unwrap()
            .committed_headers_page(None, 128)
            .unwrap();
        f.book.discover_receives(None, &first, own).unwrap();
        assert!(f.book.download_work_pending().unwrap());
        let after = f.book.receive_after().unwrap();
        let last = log
            .repository()
            .unwrap()
            .committed_headers_page(after, 128)
            .unwrap();
        f.book.discover_receives(after, &last, own).unwrap();
        assert!(
            !f.book.download_work_pending().unwrap(),
            "a caught-up empty scheduler must return to the idle backoff"
        );
        assert_eq!(log.repository().unwrap().body_read_counts(), (0, 0));
    }

    #[test]
    fn file_receive_discovery_preserves_account_epochs_and_undiscovered_work() {
        use crate::eventlog::{Author, persist::PersistentEventLog};
        use crate::storage::profile::participant;
        let f = Fixture::new();
        let mut log = PersistentEventLog::open_profile(f.book.profile.clone(), "pw").unwrap();
        let sender = DeviceIdentity::generate();
        let conversation = ConversationId::new([19; 32]);
        let make = |seq| {
            Event::new(
                &sender,
                conversation,
                seq,
                vec![],
                seq,
                1,
                EventKind::FileManifest,
                vec![1],
            )
        };
        let events: Vec<_> = (1..=4).map(make).collect();
        for event in &events[..2] {
            log.append_durable(event.clone()).unwrap();
        }
        let own = Author::from_ed25519(f.device.public().ed25519_pub);
        let first = log
            .repository()
            .unwrap()
            .committed_headers_page(None, 1)
            .unwrap();
        f.book.discover_receives(None, &first, own).unwrap();
        let next = Account::generate();
        participant::prepare_adoption(
            f.book.profile.database(),
            &f.account.public(),
            &next.public(),
        )
        .unwrap();
        participant::finish_adoption(f.book.profile.database(), &next.public()).unwrap();
        log.append_durable(events[2].clone()).unwrap();
        let b = IndexedFileBook::open(f.book.profile.clone()).unwrap();
        b.discover_receives(
            None,
            &log.repository()
                .unwrap()
                .committed_headers_page(None, 128)
                .unwrap(),
            own,
        )
        .unwrap();
        assert_eq!(
            b.receive_candidates(conversation, 128, 8 * 1024 * 1024)
                .unwrap(),
            vec![events[2].id],
            "the adopted account must not receive old-account file notices"
        );
        participant::prepare_adoption(
            f.book.profile.database(),
            &next.public(),
            &f.account.public(),
        )
        .unwrap();
        participant::finish_adoption(f.book.profile.database(), &f.account.public()).unwrap();
        log.append_durable(events[3].clone()).unwrap();
        let a = IndexedFileBook::open(f.book.profile.clone()).unwrap();
        let after = a.receive_after().unwrap();
        a.discover_receives(
            after,
            &log.repository()
                .unwrap()
                .committed_headers_page(after, 128)
                .unwrap(),
            own,
        )
        .unwrap();
        let selected: std::collections::HashSet<_> = a
            .receive_candidates(conversation, 128, 8 * 1024 * 1024)
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(
            selected,
            std::collections::HashSet::from([events[0].id, events[1].id, events[3].id])
        );
        assert!(matches!(b.receive_after(), Err(LogError::StaleCursor)));
        assert!(
            matches!(f.book.receive_after(), Err(LogError::StaleCursor)),
            "returning to A must not revive an old A scheduler"
        );
        assert!(
            matches!(
                f.book.forget_receive(events[0].id),
                Err(LogError::StaleCursor)
            ),
            "an old A acknowledgement must not retire resumed A work"
        );
    }
}

#[cfg(test)]
impl IndexedFileBook {
    pub(crate) fn file_convs(&self) -> Vec<ConversationId> {
        self.files().collect::<Result<_, _>>().unwrap()
    }
    pub(crate) fn manifest_events(&self, file: ConversationId) -> Vec<EventId> {
        self.offers(Some(file), None)
            .map(|offer| offer.map(|offer| offer.event))
            .collect::<Result<_, _>>()
            .unwrap()
    }
    pub(crate) fn is_emitted(&self, id: &EventId) -> bool {
        self.processed(*id).unwrap()
    }
    pub(crate) fn pending_for_test(&self, file: ConversationId) -> bool {
        self.profile.database().read(|c|Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM file_download_pending WHERE account=?1 AND file=?2)",params![self.account.as_slice(),file.as_bytes().as_slice()],|r|r.get(0))?)).unwrap()
    }
}
