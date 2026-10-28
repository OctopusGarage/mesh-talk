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
const BATCH_ROWS: usize = 64;
const BATCH_BYTES: usize = 1024 * 1024;
// The NULL rank uniqueness index otherwise tempts SQLite to sort every ready
// vertex on every pop. The owned ready index bounds the ordered prefix scan.
const NEXT_READY: &str = "SELECT id,EXISTS(SELECT 1 FROM relay_import_edges INDEXED BY relay_import_children WHERE parent=relay_import_events.id) FROM relay_import_events INDEXED BY relay_import_ready WHERE rank IS NULL AND remaining=0 ORDER BY ordinal,id LIMIT 64";
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
// Retain encoded rows only. The source visitor owns the sole decoded Event.
// Charge reserved fixed metadata and parent IDs as well as immutable bytes.
struct EncodedRow {
    id: EventId,
    conversation: ConversationId,
    author: Author,
    seq: u64,
    ordinal: i64,
    body: Vec<u8>,
    parents: Vec<EventId>,
}
fn flush(db: &EncryptedDatabase, batch: &mut Vec<EncodedRow>) -> Result<(), LogError> {
    if batch.is_empty() {
        return Ok(());
    }
    db.transaction(|tx| {
        for row in batch.iter() {
            let existing: Option<bool> = tx.query_row(
                "SELECT body=?1 FROM relay_import_events WHERE id=?2",
                params![row.body, row.id.as_bytes().as_slice()], |r| r.get(0),
            ).optional()?;
            if let Some(equal) = existing {
                if !equal { return Err(invalid("duplicate event identity has different immutable bytes")); }
                continue;
            }
            let equivocation: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM relay_import_events WHERE conversation=?1 AND author=?2 AND seq=?3)",
                params![row.conversation.as_bytes().as_slice(), row.author.ed25519_pub().as_slice(), row.seq.to_be_bytes().as_slice()], |r| r.get(0),
            )?;
            if equivocation { return Err(LogError::AuthorEquivocation {author: row.author, seq: row.seq}); }
            tx.execute("INSERT INTO relay_import_events VALUES(?1,?2,?3,?4,?5,?6,?7,NULL)", params![row.id.as_bytes().as_slice(), row.conversation.as_bytes().as_slice(), row.author.ed25519_pub().as_slice(), row.seq.to_be_bytes().as_slice(), row.ordinal, row.body, integer(row.parents.len())?])?;
            for parent in &row.parents {
                tx.execute("INSERT INTO relay_import_edges VALUES(?1,?2)", params![row.id.as_bytes().as_slice(),parent.as_bytes().as_slice()])?;
            }
        }
        Ok(())
    })?;
    #[cfg(test)]
    crate::storage::profile::publication::tests::MAX_BATCH
        .with(|v| v.set(v.get().max(batch.len())));
    batch.clear();
    observe("batch");
    cut("event-batch")
}
pub(crate) fn stage(db: &EncryptedDatabase, path: &Path, password: &str) -> Result<i64, LogError> {
    clear(db)?;
    let mut records = 0i64;
    let mut batch = Vec::with_capacity(BATCH_ROWS);
    let overhead = batch
        .capacity()
        .checked_mul(std::mem::size_of::<EncodedRow>())
        .and_then(|n| n.checked_add(std::mem::size_of_val(&batch)))
        .ok_or(LogError::RepositoryCounterExhausted)?;
    let mut bytes = overhead;
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
            let encoded = bincode::serialized_size(&event)
                .map_err(|e| LogError::Serialization(e.to_string()))?;
            let charge = usize::try_from(encoded)
                .ok()
                .and_then(|n| {
                    event
                        .parents
                        .capacity()
                        .checked_mul(std::mem::size_of::<EventId>())
                        .and_then(|p| n.checked_add(p))
                })
                .ok_or(LogError::RepositoryCounterExhausted)?;
            // Flush before serializing the next row. A single legal oversized
            // row is admitted alone and flushed immediately, never accumulated.
            if !batch.is_empty() && charge > BATCH_BYTES.saturating_sub(bytes) {
                flush(db, &mut batch)?;
                bytes = overhead;
            }
            let body =
                bincode::serialize(&event).map_err(|e| LogError::Serialization(e.to_string()))?;
            batch.push(EncodedRow {
                id: event.id,
                conversation: event.conversation_id,
                author: event.author,
                seq: event.seq,
                ordinal: records,
                body,
                parents: event.parents,
            });
            bytes = bytes
                .checked_add(charge)
                .ok_or(LogError::RepositoryCounterExhausted)?;
            records = records
                .checked_add(1)
                .ok_or(LogError::RepositoryCounterExhausted)?;
            if batch.len() == BATCH_ROWS || bytes >= BATCH_BYTES {
                flush(db, &mut batch)?;
                bytes = overhead;
            }
            Ok(())
        },
    )?);
    flush(db, &mut batch)?;
    Ok(records)
}
pub(crate) fn rank(db: &EncryptedDatabase) -> Result<(), LogError> {
    db.read(|c| {
        let missing:Option<Vec<u8>>=c.query_row("SELECT e.parent FROM relay_import_edges e JOIN relay_import_events child ON child.id=e.child LEFT JOIN relay_import_events parent ON parent.id=e.parent AND parent.conversation=child.conversation WHERE parent.id IS NULL LIMIT 1",[],|r|r.get(0)).optional()?;
        if let Some(id)=missing {return Err(LogError::MissingParents(vec![EventId::new(id.try_into().map_err(|_|invalid("invalid staged parent"))?)]));} Ok(())
    })?;
    let mut rank = 0i64;
    loop {
        let (leaves, parent) = db.transaction(|tx| {
            let (leaves, parent) = {
                let mut stmt = tx.prepare(NEXT_READY)?;
                let mut rows = stmt.query([])?;
                // Only fixed IDs are retained: 64 * 32 bytes plus the Vec
                // header. SQLite can still touch co-located body pages while
                // seeking this unchanged WITHOUT ROWID staging table; the bound
                // is retained Rust metadata, not exceptional-migration I/O.
                let mut leaves = Vec::<[u8; 32]>::with_capacity(BATCH_ROWS);
                let mut parent = None;
                while let Some(row) = rows.next()? {
                    let id: [u8; 32] = row
                        .get::<_, Vec<u8>>(0)?
                        .try_into()
                        .map_err(|_| invalid("invalid staged event identity"))?;
                    let has_children: bool = row.get(1)?;
                    if has_children {
                        if leaves.is_empty() {
                            parent = Some(id);
                        }
                        break;
                    }
                    leaves.push(id);
                }
                (leaves, parent)
            }; // Drop the SELECT cursor before any write.
            if let Some(id) = &parent {
                tx.execute(
                    "UPDATE relay_import_events SET rank=?1 WHERE id=?2",
                    params![rank, id.as_slice()],
                )?;
            } else {
                for (offset, id) in leaves.iter().enumerate() {
                    let next = rank
                        .checked_add(integer(offset)?)
                        .ok_or(LogError::RepositoryCounterExhausted)?;
                    tx.execute(
                        "UPDATE relay_import_events SET rank=?1 WHERE id=?2",
                        params![next, id.as_slice()],
                    )?;
                }
            }
            Ok((leaves.len(), parent))
        })?;
        if leaves > 0 {
            // Removing childless ready vertices cannot add a newly ready vertex.
            // Stop before any parent: it may release an earlier-priority child.
            rank = rank
                .checked_add(integer(leaves)?)
                .ok_or(LogError::RepositoryCounterExhausted)?;
            cut("kahn-batch")?;
            continue;
        }
        let Some(id) = parent else {
            break;
        };
        // Persisted rank and bounded child pages; interruption restarts parsing
        // rather than pretending this is a seek-resumable source offset.
        let mut after = Vec::<u8>::new();
        loop {
            let children=db.transaction(|tx| {
                let children={let mut stmt=tx.prepare("SELECT child FROM relay_import_edges WHERE parent=?1 AND child>?2 ORDER BY child LIMIT ?3")?;let rows=stmt.query_map(params![id.as_slice(),after,PAGE as i64],|r|r.get::<_,Vec<u8>>(0))?;rows.collect::<Result<Vec<_>,_>>()?};
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
        if repository.append_on(c, &event)? != AppendOutcome::Appended {
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
    fn database() -> (tempfile::TempDir, EncryptedDatabase) {
        let root = tempfile::tempdir().unwrap();
        let db =
            EncryptedDatabase::open(&root.path().join("import.db"), "pw", b"import-batch").unwrap();
        initialize(&db).unwrap();
        (root, db)
    }
    fn staged_count(db: &EncryptedDatabase) -> i64 {
        db.read(|c| Ok(c.query_row("SELECT count(*) FROM relay_import_events", [], |r| r.get(0))?))
            .unwrap()
    }
    fn legacy(path: &Path, count: u64, equivocation: bool, payload: usize) {
        use crate::{eventlog::EventKind, identity::device::DeviceIdentity};
        let device = DeviceIdentity::generate();
        let (mut log, _) = EncryptedRecordLog::<Event>::open(path, "pw", b"MTLOG1").unwrap();
        for i in 0..count {
            let event = Event::new(
                &device,
                ConversationId::new([3; 32]),
                if equivocation { 0 } else { i },
                vec![],
                i,
                i,
                EventKind::Message,
                vec![7; payload],
            );
            log.append(&event).unwrap();
        }
    }
    #[test]
    fn stage_commits_bounded_encoded_batches() {
        use crate::storage::profile::publication::tests::CUT;
        let (root, db) = database();
        let path = root.path().join("legacy");
        legacy(&path, 65, false, 16);
        CUT.with(|v| v.replace(Some("event-batch")));
        assert!(stage(&db, &path, "pw").is_err());
        assert_eq!(staged_count(&db), 64, "crash cut follows a committed batch");
        assert_eq!(stage(&db, &path, "pw").unwrap(), 65);
        assert_eq!(staged_count(&db), 65);
    }
    #[test]
    fn stage_rolls_back_the_whole_failed_batch() {
        let (root, db) = database();
        let path = root.path().join("legacy");
        legacy(&path, 2, true, 16);
        assert!(matches!(
            stage(&db, &path, "pw"),
            Err(LogError::AuthorEquivocation { .. })
        ));
        assert_eq!(staged_count(&db), 0);
        assert!(!db.requires_reopen());
    }
    fn insert_vertex(c: &Connection, ordinal: i64, size: usize) -> Result<Vec<u8>, LogError> {
        let mut id = vec![0; 32];
        id[..8].copy_from_slice(&ordinal.to_be_bytes());
        c.execute(
            "INSERT INTO relay_import_events VALUES(?1,?2,?3,?4,?5,?6,0,NULL)",
            params![
                id,
                [1u8; 32].as_slice(),
                [2u8; 32].as_slice(),
                ordinal.to_be_bytes().as_slice(),
                ordinal,
                vec![0u8; size]
            ],
        )?;
        Ok(id)
    }
    fn rank_database() -> (
        tempfile::TempDir,
        Arc<crate::storage::profile::context::ProfileStorage>,
    ) {
        use crate::{
            identity::device::DeviceIdentity,
            storage::profile::{ProfileStorageLease, context::ProfileStorage},
        };
        let root = tempfile::tempdir().unwrap();
        let device = DeviceIdentity::generate();
        let profile = ProfileStorage::open_relay(
            ProfileStorageLease::acquire(root.path()).unwrap(),
            &device.public(),
            &root.path().join("legacy"),
            "pw",
            1,
        )
        .unwrap();
        (root, profile)
    }
    #[test]
    fn rank_commits_bounded_leaf_prefix() {
        use crate::storage::profile::publication::tests::CUT;
        let (_root, profile) = rank_database();
        let db = profile.database();
        db.transaction(|tx| {
            for i in 0..65 {
                insert_vertex(tx, i, 16)?;
            }
            Ok(())
        })
        .unwrap();
        CUT.with(|v| v.replace(Some("kahn-batch")));
        assert!(
            matches!(rank(db), Err(LogError::CorruptFile(message)) if message == "injected publication interruption")
        );
        let ranked: i64 = db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM relay_import_events WHERE rank=ordinal",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(ranked, 64);
        CUT.with(|v| v.replace(None));
        clear(db).unwrap();
        db.transaction(|tx| {
            for i in 0..65 {
                insert_vertex(tx, i, 16)?;
            }
            Ok(())
        })
        .unwrap();
        rank(db).unwrap();
        assert_eq!(
            db.read(|c| Ok(c.query_row(
                "SELECT unique_records FROM relay_sources WHERE source=0",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
            65
        );
    }
    #[test]
    fn encoded_batch_byte_budget_and_single_oversized_row() {
        use crate::storage::profile::publication::tests::CUT;
        for payload in [600 * 1024, 2 * 1024 * 1024] {
            let (root, db) = database();
            let path = root.path().join("legacy");
            legacy(&path, 2, false, payload);
            CUT.with(|v| v.replace(Some("event-batch")));
            assert!(stage(&db, &path, "pw").is_err());
            assert_eq!(staged_count(&db), 1);
            assert_eq!(stage(&db, &path, "pw").unwrap(), 2);
        }
    }
    #[test]
    fn rank_does_not_skip_parent_before_earlier_priority_child() {
        let (_root, profile) = rank_database();
        let db = profile.database();
        db.transaction(|tx| {
            let child = insert_vertex(tx, 0, 16)?;
            insert_vertex(tx, 1, 16)?;
            let parent = insert_vertex(tx, 2, 16)?;
            insert_vertex(tx, 3, 16)?;
            tx.execute(
                "UPDATE relay_import_events SET remaining=1 WHERE id=?1",
                [&child],
            )?;
            tx.execute(
                "INSERT INTO relay_import_edges VALUES(?1,?2)",
                params![child, parent],
            )?;
            Ok(())
        })
        .unwrap();
        rank(db).unwrap();
        let actual = db
            .read(|c| {
                let mut stmt =
                    c.prepare("SELECT ordinal FROM relay_import_events ORDER BY rank")?;
                Ok(stmt
                    .query_map([], |r| r.get::<_, i64>(0))?
                    .collect::<Result<Vec<_>, _>>()?)
            })
            .unwrap();
        assert_eq!(actual, vec![1, 2, 0, 3]);
    }
    #[test]
    fn rank_wide_fanout_keeps_bounded_child_pages() {
        use crate::storage::profile::publication::tests::MAX_EDGES;
        let (_root, profile) = rank_database();
        let db = profile.database();
        db.transaction(|tx| {
            let parent = insert_vertex(tx, 300, 16)?;
            for ordinal in 0..300 {
                let child = insert_vertex(tx, ordinal, 16)?;
                tx.execute(
                    "UPDATE relay_import_events SET remaining=1 WHERE id=?1",
                    [&child],
                )?;
                tx.execute(
                    "INSERT INTO relay_import_edges VALUES(?1,?2)",
                    params![child, parent],
                )?;
            }
            Ok(())
        })
        .unwrap();
        MAX_EDGES.with(|v| v.set(0));
        rank(db).unwrap();
        assert_eq!(MAX_EDGES.with(|v| v.get()), 128);
        assert_eq!(
            db.read(|c| Ok(c.query_row(
                "SELECT ordinal FROM relay_import_events WHERE rank=0",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
            300
        );
        assert_eq!(
            db.read(|c| Ok(c.query_row(
                "SELECT count(*) FROM relay_import_events WHERE remaining=0 AND rank IS NOT NULL",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
            301
        );
    }
    #[test]
    fn rank_retains_fixed_metadata_independent_of_body_size() {
        use crate::storage::profile::publication::tests::CUT;
        for size in [600 * 1024, 2 * 1024 * 1024] {
            let (_root, profile) = rank_database();
            let db = profile.database();
            db.transaction(|tx| {
                for i in 0..2 {
                    insert_vertex(tx, i, size)?;
                }
                Ok(())
            })
            .unwrap();
            CUT.with(|v| v.replace(Some("kahn-batch")));
            assert!(
                matches!(rank(db),Err(LogError::CorruptFile(message)) if message == "injected publication interruption")
            );
            assert_eq!(
                db.read(|c| Ok(c.query_row(
                    "SELECT count(*) FROM relay_import_events WHERE rank IS NOT NULL",
                    [],
                    |r| r.get::<_, i64>(0)
                )?))
                .unwrap(),
                2
            );
        }
    }
    #[test]
    fn ready_prefix_metadata_work_is_bounded_for_tiny_and_large_bodies() {
        let mut work = Vec::new();
        for size in [1, 64 * 1024] {
            let (_root, db) = database();
            db.transaction(|tx| {
                for i in 0..128 {
                    insert_vertex(tx, i, size)?;
                }
                Ok(())
            })
            .unwrap();
            let steps = db
                .read(|c| {
                    let mut stmt = c.prepare(NEXT_READY)?;
                    let mut rows = stmt.query([])?;
                    let mut count = 0;
                    while let Some(row) = rows.next()? {
                        let id: Vec<u8> = row.get(0)?;
                        assert_eq!(id.len(), 32);
                        assert!(!row.get::<_, bool>(1)?);
                        count += 1;
                    }
                    assert_eq!(count, 64);
                    drop(rows);
                    assert_eq!(stmt.get_status(StatementStatus::Sort), 0);
                    Ok(stmt.get_status(StatementStatus::VmStep))
                })
                .unwrap();
            assert!(
                steps <= 4096,
                "metadata prefix exceeded bounded work: {steps}"
            );
            work.push(steps);
        }
        assert_eq!(
            work[0], work[1],
            "BLOB size must not change metadata selector VM work"
        );
    }
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
