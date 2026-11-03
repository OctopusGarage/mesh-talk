//! Account ownership of participant event commit intervals. This is part of
//! participant adoption, not a receive queue or an additional recovery journal.
use super::{AccountPublic, LogError, invalid};
use rusqlite::{Connection, OptionalExtension, params};

const DOMAIN: &[u8; 16] = b"MeshPartEventsV1";
const SCHEMA: &str = "
CREATE TABLE participant_event_ownership(singleton INTEGER PRIMARY KEY CHECK(singleton=1),version INTEGER NOT NULL CHECK(version=1),generation INTEGER NOT NULL CHECK(typeof(generation)='integer' AND generation>=0));
CREATE TABLE participant_event_ranges(start_commit INTEGER PRIMARY KEY CHECK(typeof(start_commit)='integer' AND start_commit>=0),end_commit INTEGER CHECK(end_commit IS NULL OR (typeof(end_commit)='integer' AND end_commit>start_commit)),account_id TEXT NOT NULL CHECK(length(account_id)=32),FOREIGN KEY(account_id) REFERENCES participant_account_namespaces(account_id));
CREATE INDEX participant_event_account_ranges ON participant_event_ranges(account_id,start_commit);
CREATE UNIQUE INDEX participant_event_open_range ON participant_event_ranges((1)) WHERE end_commit IS NULL;
";
const MISSING_BOUNDARIES: &str = "participant event ownership boundaries are unavailable for this experimental multi-account profile; data was preserved";

/// Resolve the authenticated owner of a device-local legacy projection.
pub(crate) fn legacy_source_namespace_on(c: &Connection, source: &str) -> Result<String, LogError> {
    let previous: Option<String> = c.query_row(
        "SELECT bootstrap_previous FROM participant_profile WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if let Some(previous) = previous {
        return Ok(previous);
    }
    let count: i64 = c.query_row(
        "SELECT count(*) FROM (SELECT 1 FROM participant_account_namespaces LIMIT 2)",
        [],
        |r| r.get(0),
    )?;
    if count != 1 {
        return Err(invalid(&format!(
            "{source} ownership is unavailable for this experimental multi-account legacy source; data was preserved",
        )));
    }
    let (account, _, _) = super::state_on(c)?;
    let exact:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM participant_account_namespaces WHERE account_id=?1 AND account=?2)",params![account.account_id(),account.ed25519_pub.as_slice()],|r|r.get(0))?;
    if !exact {
        return Err(invalid(
            "legacy source initial namespace is not authenticated",
        ));
    }
    Ok(account.account_id())
}
/// Captured after adoption finishes. The generation also fences A -> B -> A.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParticipantEventOwner {
    account: [u8; 32],
    generation: i64,
}
impl ParticipantEventOwner {
    pub(crate) fn account(&self) -> &[u8; 32] {
        &self.account
    }
    pub(crate) fn namespace(&self) -> String {
        AccountPublic {
            ed25519_pub: self.account,
        }
        .account_id()
    }
}

pub(super) fn initialize_fresh_on(c: &Connection, namespace: &str) -> Result<(), LogError> {
    c.execute_batch(SCHEMA)?;
    c.execute("INSERT INTO participant_event_ownership VALUES(1,1,0)", [])?;
    c.execute(
        "INSERT INTO participant_event_ranges VALUES(0,NULL,?1)",
        [namespace],
    )?;
    Ok(())
}

/// Upgrade only when the existing owner supplies an exact initial namespace.
/// A namespace roster cannot reconstruct lost historical adoption boundaries.
pub(super) fn initialize_existing_on(c: &Connection) -> Result<(), LogError> {
    let present: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='participant_event_ownership')",
        [],
        |r| r.get(0),
    )?;
    if present {
        return validate_on(c);
    }
    let (active, _, previous) = super::state_on(c)?;
    let namespaces: i64 = c.query_row(
        "SELECT count(*) FROM (SELECT 1 FROM participant_account_namespaces LIMIT 3)",
        [],
        |r| r.get(0),
    )?;
    if let Some(previous) = previous {
        if namespaces != 2 {
            return Err(invalid(MISSING_BOUNDARIES));
        }
        // The initial legacy import counter is an authoritative cut, unlike a
        // later roster of retained account names. No other old cut is inferred.
        initialize_fresh_on(c, &previous)?;
        let publication: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='participant_event_publication')",
            [],
            |r| r.get(0),
        )?;
        if publication {
            let (phase, through): (i64, i64) = c.query_row(
                "SELECT phase,unique_records FROM participant_event_publication WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if phase == 4 {
                rotate_on(c, &previous, &active.account_id(), through)?;
            }
        }
    } else {
        if namespaces != 1 {
            return Err(invalid(MISSING_BOUNDARIES));
        }
        let exact: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM participant_account_namespaces WHERE account_id=?1 AND account=?2)", params![active.account_id(),active.ed25519_pub.as_slice()], |r| r.get(0))?;
        if !exact {
            return Err(invalid(
                "participant initial namespace does not match its authenticated account",
            ));
        }
        initialize_fresh_on(c, &active.account_id())?;
    }
    validate_on(c)
}

fn validate_on(c: &Connection) -> Result<(), LogError> {
    super::manifest::validate_ddl(c, SCHEMA)?;
    let (version, generation): (i64, i64) = c.query_row(
        "SELECT version,generation FROM participant_event_ownership WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if version != 1 || generation < 0 {
        return Err(invalid("invalid participant event ownership generation"));
    }
    let (active, _, previous) = super::state_on(c)?;
    let (start, namespace): (i64, String) = c.query_row(
        "SELECT start_commit,account_id FROM participant_event_ranges WHERE end_commit IS NULL",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if start < 0
        || (namespace != active.account_id() && previous.as_deref() != Some(namespace.as_str()))
    {
        return Err(invalid(
            "participant event range disagrees with account authority",
        ));
    }
    let initial: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM participant_event_ranges WHERE start_commit=0)",
        [],
        |r| r.get(0),
    )?;
    if !initial {
        return Err(invalid("participant event ownership has no initial range"));
    }
    Ok(())
}

fn next_commit_on(c: &Connection) -> Result<i64, LogError> {
    Ok(c.query_row(
        "SELECT next_commit FROM event_domains WHERE domain=?1",
        [DOMAIN.as_slice()],
        |r| r.get(0),
    )?)
}

fn rotate_on(c: &Connection, previous: &str, next: &str, through: i64) -> Result<(), LogError> {
    if previous == next {
        return Ok(());
    }
    let (start, namespace): (i64, String) = c.query_row(
        "SELECT start_commit,account_id FROM participant_event_ranges WHERE end_commit IS NULL",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if namespace != previous || through < start || through > next_commit_on(c)? {
        return Err(invalid("participant event adoption boundary changed"));
    }
    let generation: i64 = c.query_row(
        "SELECT generation FROM participant_event_ownership WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    let generation = generation
        .checked_add(1)
        .ok_or(LogError::RepositoryCounterExhausted)?;
    if through == start {
        c.execute("UPDATE participant_event_ranges SET account_id=?1 WHERE start_commit=?2 AND end_commit IS NULL", params![next,start])?;
    } else {
        c.execute("UPDATE participant_event_ranges SET end_commit=?1 WHERE start_commit=?2 AND end_commit IS NULL", params![through,start])?;
        c.execute(
            "INSERT INTO participant_event_ranges VALUES(?1,NULL,?2)",
            params![through, next],
        )?;
    }
    c.execute(
        "UPDATE participant_event_ownership SET generation=?1 WHERE singleton=1",
        [generation],
    )?;
    Ok(())
}

pub(super) fn check_active_range_on(
    c: &Connection,
    loaded: &AccountPublic,
) -> Result<(), LogError> {
    let namespace: String = c.query_row(
        "SELECT account_id FROM participant_event_ranges WHERE end_commit IS NULL",
        [],
        |r| r.get(0),
    )?;
    if namespace != loaded.account_id() {
        return Err(invalid(
            "participant imported event ownership is not published",
        ));
    }
    Ok(())
}

pub(super) fn adopt_on(
    c: &Connection,
    previous: &AccountPublic,
    next: &AccountPublic,
) -> Result<(), LogError> {
    rotate_on(
        c,
        &previous.account_id(),
        &next.account_id(),
        next_commit_on(c)?,
    )
}

/// Called in the same transaction that publishes the imported participant rows.
/// New loaded-account appends cannot become part of the previous owner's prefix.
pub(in crate::storage::profile) fn finish_import_on(c: &Connection) -> Result<(), LogError> {
    let (active, _, previous) = super::state_on(c)?;
    if let Some(previous) = previous {
        rotate_on(c, &previous, &active.account_id(), next_commit_on(c)?)?;
    }
    Ok(())
}

pub(crate) fn capture_event_owner_on(
    c: &Connection,
    loaded: &AccountPublic,
) -> Result<ParticipantEventOwner, LogError> {
    let (active, _, previous) = super::state_on(c)?;
    if &active != loaded || previous.is_some() {
        return Err(LogError::StaleCursor);
    }
    validate_on(c)?;
    let generation = c.query_row(
        "SELECT generation FROM participant_event_ownership WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    Ok(ParticipantEventOwner {
        account: loaded.ed25519_pub,
        generation,
    })
}

pub(crate) fn check_event_owner_on(
    c: &Connection,
    owner: &ParticipantEventOwner,
) -> Result<(), LogError> {
    let current: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM participant_profile p JOIN participant_event_ownership o ON o.singleton=p.singleton WHERE p.singleton=1 AND p.account=?1 AND p.bootstrap_previous IS NULL AND o.generation=?2)", params![owner.account.as_slice(),owner.generation], |r| r.get(0))?;
    if !current {
        return Err(LogError::StaleCursor);
    }
    Ok(())
}

/// Derived schedulers may store this namespace beside a pending key so their
/// account index qualifies work before LIMIT. This range remains the authority.
pub(crate) fn participant_event_namespace_on(
    c: &Connection,
    ordinal: i64,
) -> Result<String, LogError> {
    if ordinal < 0 || ordinal >= next_commit_on(c)? {
        return Err(invalid(
            "participant event ordinal is outside committed history",
        ));
    }
    let range: Option<(Option<i64>,String)> = c.query_row("SELECT end_commit,account_id FROM participant_event_ranges WHERE start_commit<=?1 ORDER BY start_commit DESC LIMIT 1", [ordinal], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((end, namespace)) = range else {
        return Err(invalid("participant event ownership range is missing"));
    };
    if end.is_some_and(|end| ordinal >= end) {
        return Err(invalid("participant event ownership range has a gap"));
    }
    Ok(namespace)
}

pub(crate) fn participant_event_owned_on(
    c: &Connection,
    owner: &ParticipantEventOwner,
    ordinal: i64,
) -> Result<bool, LogError> {
    check_event_owner_on(c, owner)?;
    let namespace = participant_event_namespace_on(c, ordinal)?;
    Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM participant_account_namespaces WHERE account_id=?1 AND account=?2)", params![namespace,owner.account.as_slice()], |r| r.get(0))?)
}
