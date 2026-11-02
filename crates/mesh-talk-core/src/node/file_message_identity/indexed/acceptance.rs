//! Current accepted bindings carry their insertion revision. This is a selector
//! over immutable authority, not a second history journal or a body cache.
use super::*;

const SCHEMA: &str = "
CREATE TABLE file_identity_acceptance(host BLOB NOT NULL,logical BLOB NOT NULL,revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=0),PRIMARY KEY(host,logical),FOREIGN KEY(host,logical) REFERENCES file_identity_headers(host,logical) ON DELETE CASCADE) WITHOUT ROWID;
CREATE INDEX file_identity_acceptance_order ON file_identity_acceptance(host,revision,logical);
";

pub(super) fn initialize_on(c: &Connection) -> Result<(), LogError> {
    let present: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='file_identity_acceptance')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        c.execute_batch(SCHEMA)?;
        // Upgrades establish a baseline using only fixed-size header keys.
        c.execute(
            "INSERT INTO file_identity_acceptance SELECT host,logical,0 FROM file_identity_headers",
            [],
        )?;
    }
    manifest::validate_ddl(c, SCHEMA)
}

pub(super) fn insert_on(
    c: &Connection,
    host: ConversationId,
    logical: EventId,
    revision: i64,
) -> Result<(), LogError> {
    c.execute(
        "INSERT INTO file_identity_acceptance VALUES(?1,?2,?3)",
        params![
            host.as_bytes().as_slice(),
            logical.as_bytes().as_slice(),
            revision
        ],
    )?;
    Ok(())
}

pub(crate) struct AcceptedFileIdentity {
    pub revision: u64,
    pub logical: EventId,
}

impl IndexedFileMessageIdentities {
    /// Exact aliases of an already accepted immutable binding, without loading
    /// its signed bundle again. Admission limits the binding to 64 aliases.
    pub(crate) fn accepted_aliases(
        &self,
        host: ConversationId,
        logical: EventId,
    ) -> Result<Vec<EventId>, LogError> {
        self.source.profile().database().read(|c| {
            let mut statement = c.prepare_cached("SELECT manifest FROM file_identity_aliases WHERE host=?1 AND logical=?2 ORDER BY manifest LIMIT ?3")?;
            let mut rows = statement.query(params![host.as_bytes().as_slice(),logical.as_bytes().as_slice(),(MAX_ALIASES + 1) as i64])?;
            let mut aliases = Vec::new();
            while let Some(row) = rows.next()? {
                if aliases.len() == MAX_ALIASES { return Err(invalid("invalid accepted alias count")); }
                aliases.push(EventId::new(blob(row,0)?));
            }
            Ok(aliases)
        })
    }

    /// Seek a bounded number of newly accepted bindings in this exact host.
    /// Legacy baseline bindings have revision zero and are covered by the first
    /// history snapshot; no signed payload is read by this selector.
    pub(crate) fn accepted_page(
        &self,
        host: ConversationId,
        after_revision: u64,
        through_revision: u64,
        rows: usize,
    ) -> Result<Vec<AcceptedFileIdentity>, LogError> {
        if rows == 0 || rows > 128 || through_revision < after_revision {
            return Err(LogError::InvalidPageBudget);
        }
        let after = i64::try_from(after_revision).map_err(|_| LogError::InvalidPageBudget)?;
        let through = i64::try_from(through_revision).map_err(|_| LogError::InvalidPageBudget)?;
        self.source.profile().database().read(|c| {
            let mut statement = c.prepare_cached("SELECT revision,logical FROM file_identity_acceptance WHERE host=?1 AND revision>?2 AND revision<=?3 ORDER BY revision,logical LIMIT ?4")?;
            let mut found=statement.query(params![host.as_bytes().as_slice(),after,through,rows as i64])?;
            let mut page=Vec::new();
            while let Some(row)=found.next()? {
                let revision:i64=row.get(0)?;
                page.push(AcceptedFileIdentity {revision:u64::try_from(revision).map_err(|_| invalid("invalid identity acceptance revision"))?,logical:EventId::new(blob(row,1)?)});
            }
            Ok(page)
        })
    }
}
