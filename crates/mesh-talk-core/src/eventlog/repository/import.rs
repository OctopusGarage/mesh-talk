//! Exceptional migration staging. All history-sized state lives in ordinary
//! encrypted SQL tables. Restart abandons prior staging in bounded delete pages;
//! the shared legacy visitor supplies no authenticated seek/resume boundary.
use super::*;
use crate::storage::{
    profile::{
        manifest::{invalid, validate_ddl},
        publication::{DecodedRecord, cut, observe, observe_edges},
    },
    record_log::EncryptedRecordLog,
};
use std::path::Path;
const PAGE: usize = 128;
// The NULL rank uniqueness index otherwise tempts SQLite to sort every ready
// vertex on every pop. This owned covering index is part of the work bound.
const NEXT_READY: &str = "SELECT id FROM relay_import_events INDEXED BY relay_import_ready WHERE rank IS NULL AND remaining=0 ORDER BY ordinal,id LIMIT 1";
const SCHEMA:&str="
CREATE TABLE relay_import_events(id BLOB PRIMARY KEY CHECK(length(id)=32), conversation BLOB NOT NULL CHECK(length(conversation)=32), author BLOB NOT NULL CHECK(length(author)=32), seq BLOB NOT NULL CHECK(length(seq)=8), ordinal INTEGER NOT NULL UNIQUE CHECK(ordinal>=0), body BLOB NOT NULL, remaining INTEGER NOT NULL CHECK(remaining>=0), rank INTEGER UNIQUE CHECK(rank IS NULL OR rank>=0), UNIQUE(conversation,author,seq)) WITHOUT ROWID;
CREATE INDEX relay_import_ready ON relay_import_events(remaining,ordinal,id) WHERE rank IS NULL;
CREATE TABLE relay_import_edges(child BLOB NOT NULL CHECK(length(child)=32), parent BLOB NOT NULL CHECK(length(parent)=32), PRIMARY KEY(child,parent)) WITHOUT ROWID;
CREATE INDEX relay_import_children ON relay_import_edges(parent,child);
";
pub(crate) fn initialize(db: &EncryptedDatabase) -> Result<(), LogError> {
    db.transaction(|tx| {
        tx.execute_batch(SCHEMA)?;
        Ok(())
    })
}
pub(crate) fn validate(db: &EncryptedDatabase) -> Result<(), LogError> {
    db.read(|c| validate_ddl(c, SCHEMA))
}
pub(crate) fn reclaim_step(db: &EncryptedDatabase) -> Result<bool, LogError> {
    db.transaction(|tx| {
        let edges = {
            let mut stmt = tx.prepare(
                "SELECT child,parent FROM relay_import_edges ORDER BY child,parent LIMIT ?1",
            )?;
            let rows = stmt.query_map([PAGE as i64], |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for (child, parent) in &edges {
            tx.execute(
                "DELETE FROM relay_import_edges WHERE child=?1 AND parent=?2",
                params![child, parent],
            )?;
        }
        // Edges have no FK to staging and are not queried again after VERIFIED.
        let ids = {
            let mut stmt = tx.prepare("SELECT id FROM relay_import_events ORDER BY id LIMIT ?1")?;
            let rows = stmt.query_map([PAGE as i64], |r| r.get::<_, Vec<u8>>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for id in &ids {
            tx.execute("DELETE FROM relay_import_events WHERE id=?1", [id])?;
        }
        Ok(edges.len() < PAGE && ids.len() < PAGE)
    })
}
pub(crate) fn clear(db: &EncryptedDatabase) -> Result<(), LogError> {
    while !reclaim_step(db)? {}
    Ok(())
}
pub(crate) fn stage(db: &EncryptedDatabase, path: &Path, password: &str) -> Result<i64, LogError> {
    clear(db)?;
    let mut records = 0i64;
    drop(EncryptedRecordLog::<Event>::open_with_visitor(
        path,
        password,
        b"MTLOG1",
        |event| {
            let _decoded = DecodedRecord::new();
            observe("decode");
            if !event.is_canonical() {
                return Err(LogError::NonCanonical);
            }
            if !event.verify_integrity() {
                return Err(LogError::CorruptId);
            }
            if !event.verify_signature() {
                return Err(LogError::BadSignature);
            }
            let body =
                bincode::serialize(&event).map_err(|e| LogError::Serialization(e.to_string()))?;
            db.transaction(|tx| {
            let existing:Option<bool>=tx.query_row("SELECT body=?1 FROM relay_import_events WHERE id=?2",params![body,event.id.as_bytes().as_slice()],|r|r.get(0)).optional()?;
            if let Some(equal)=existing {if !equal{return Err(invalid("duplicate event identity has different immutable bytes"));}return Ok(());}
            let equivocation:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM relay_import_events WHERE conversation=?1 AND author=?2 AND seq=?3)",params![event.conversation_id.as_bytes().as_slice(),event.author.ed25519_pub().as_slice(),event.seq.to_be_bytes().as_slice()],|r|r.get(0))?;
            if equivocation {return Err(LogError::AuthorEquivocation {author:event.author,seq:event.seq});}
            tx.execute("INSERT INTO relay_import_events VALUES(?1,?2,?3,?4,?5,?6,?7,NULL)",params![event.id.as_bytes().as_slice(),event.conversation_id.as_bytes().as_slice(),event.author.ed25519_pub().as_slice(),event.seq.to_be_bytes().as_slice(),records,body,integer(event.parents.len())?])?;
            for parent in &event.parents {tx.execute("INSERT INTO relay_import_edges VALUES(?1,?2)",params![event.id.as_bytes().as_slice(),parent.as_bytes().as_slice()])?;}
            Ok(())
        })?;
            records = records
                .checked_add(1)
                .ok_or(LogError::RepositoryCounterExhausted)?;
            observe("batch");
            cut("event-batch")?;
            Ok(())
        },
    )?);
    Ok(records)
}
pub(crate) fn rank(db: &EncryptedDatabase) -> Result<(), LogError> {
    db.read(|c| {
        let missing:Option<Vec<u8>>=c.query_row("SELECT e.parent FROM relay_import_edges e JOIN relay_import_events child ON child.id=e.child LEFT JOIN relay_import_events parent ON parent.id=e.parent AND parent.conversation=child.conversation WHERE parent.id IS NULL LIMIT 1",[],|r|r.get(0)).optional()?;
        if let Some(id)=missing {return Err(LogError::MissingParents(vec![EventId::new(id.try_into().map_err(|_|invalid("invalid staged parent"))?)]));} Ok(())
    })?;
    let mut rank = 0i64;
    loop {
        let id = db.transaction(|tx| {
            let id: Option<Vec<u8>> = tx.query_row(NEXT_READY, [], |r| r.get(0)).optional()?;
            if let Some(id) = &id {
                tx.execute(
                    "UPDATE relay_import_events SET rank=?1 WHERE id=?2",
                    params![rank, id],
                )?;
            }
            Ok(id)
        })?;
        let Some(id) = id else {
            break;
        };
        // Persisted rank and bounded child pages; interruption restarts parsing
        // rather than pretending this is a seek-resumable source offset.
        let mut after = Vec::<u8>::new();
        loop {
            let children=db.transaction(|tx| {
                let children={let mut stmt=tx.prepare("SELECT child FROM relay_import_edges WHERE parent=?1 AND child>?2 ORDER BY child LIMIT ?3")?;let rows=stmt.query_map(params![id,after,PAGE as i64],|r|r.get::<_,Vec<u8>>(0))?;rows.collect::<Result<Vec<_>,_>>()?};
                for child in &children {if tx.execute("UPDATE relay_import_events SET remaining=remaining-1 WHERE id=?1 AND remaining>0",[child])?!=1{return Err(invalid("staged DAG degree underflow"));}}
                Ok(children)
            })?;
            observe_edges(children.len());
            if let Some(last) = children.last() {
                after = last.clone();
            } else {
                break;
            }
            cut("kahn-page")?;
        }
        rank = rank
            .checked_add(1)
            .ok_or(LogError::RepositoryCounterExhausted)?;
    }
    db.transaction(|tx| {
        let count: i64 =
            tx.query_row("SELECT count(*) FROM relay_import_events", [], |r| r.get(0))?;
        if count != rank {
            return Err(invalid("staged event graph contains a cycle"));
        }
        tx.execute(
            "UPDATE relay_sources SET unique_records=?1 WHERE source=0",
            [count],
        )?;
        Ok(())
    })
}
pub(crate) fn publish(repository: &EventRepository, c: &Connection) -> Result<(), LogError> {
    let expected: i64 = c.query_row(
        "SELECT unique_records FROM relay_sources WHERE source=0 AND phase=3",
        [],
        |r| r.get(0),
    )?;
    let mut rank = 0i64;
    while rank < expected {
        // Each SELECT completes before mutating retained tables. No live SQL
        // cursor is advanced across EventRepository admission on this connection.
        let body: Vec<u8> = c.query_row(
            "SELECT body FROM relay_import_events WHERE rank=?1",
            [rank],
            |r| r.get(0),
        )?;
        let event: Event = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(body.len() as u64)
            .reject_trailing_bytes()
            .deserialize(&body)
            .map_err(|e| LogError::Serialization(e.to_string()))?;
        let _decoded = DecodedRecord::new();
        drop(body);
        if repository.insert(c, &event)? != AppendOutcome::Appended {
            return Err(invalid("final event domain was not empty"));
        }
        rank = rank
            .checked_add(1)
            .ok_or(LogError::RepositoryCounterExhausted)?;
        cut("admission-row")?;
    }
    let actual: i64 = c.query_row(
        "SELECT next_commit FROM event_domains WHERE domain=?1",
        [repository.domain.0.as_slice()],
        |r| r.get(0),
    )?;
    if actual != expected {
        return Err(invalid(
            "published event count differs from verified source",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::StatementStatus;
    #[test]
    fn ready_selector_work_is_bounded_with_many_remaining_vertices() {
        let root = tempfile::tempdir().unwrap();
        let db =
            EncryptedDatabase::open(&root.path().join("query.db"), "pw", b"query-plan-fixture")
                .unwrap();
        initialize(&db).unwrap();
        db.transaction(|tx| {
            for ordinal in 0u64..1000 {
                let mut id = [0; 32];
                id[..8].copy_from_slice(&ordinal.to_be_bytes());
                tx.execute(
                    "INSERT INTO relay_import_events VALUES(?1,?2,?3,?4,?5,?6,0,?7)",
                    params![
                        id.as_slice(),
                        [1u8; 32].as_slice(),
                        [2u8; 32].as_slice(),
                        ordinal.to_be_bytes().as_slice(),
                        ordinal as i64,
                        [0u8; 1].as_slice(),
                        if ordinal < 900 {
                            Some(ordinal as i64)
                        } else {
                            None
                        }
                    ],
                )?;
            }
            Ok(())
        })
        .unwrap();
        db.read(|c| {
            let plan = {
                let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {NEXT_READY}"))?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(3))?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            let mut stmt = c.prepare(NEXT_READY)?;
            let id: Vec<u8> = stmt.query_row([], |r| r.get(0))?;
            assert_eq!(&id[..8], &900u64.to_be_bytes());
            let steps = stmt.get_status(StatementStatus::VmStep);
            let sorts = stmt.get_status(StatementStatus::Sort);
            eprintln!("ready query plan={plan:?}; vm_steps={steps}; sorts={sorts}");
            assert!(
                steps <= 128,
                "ready lookup scanned the remaining ready set: {steps} VM steps"
            );
            assert_eq!(sorts, 0, "ready lookup must not sort retained history");
            assert!(plan.iter().any(|line| line.contains("relay_import_ready")));
            Ok(())
        })
        .unwrap();
    }
}
