//! SQLite composition, not a production database schema or network protocol.
//! One authorized scope and one connection/writer. Commits maintain a compact
//! summary index atomically with bodies. Snapshot
//! creation reads only its revision; range reads use indexed endpoint seeks. Real hosts
//! authenticate the peer and verify application signatures in the transaction.
use mesh_replication::{
    pull::Record,
    range::{Checkpoint, Prefix, RangeIndex, RangeRead, Summary, SummaryFrame},
    reader::{self, ReaderLimits, ReaderSource},
};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{error::Error, path::Path};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const SCOPE: [u8; 32] = [1; 32];
const AUTHORITY: [u8; 32] = [2; 32];
const VIEW: [u8; 32] = [3; 32];

struct Store {
    db: Connection,
    checkpoint: Option<Checkpoint>,
    limits: ReaderLimits,
}
struct TurnResult {
    committed: usize,
    complete: bool,
}
impl Store {
    fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS bodies(id BLOB PRIMARY KEY, body BLOB NOT NULL);",
        )?;
        let indexed: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='reader_meta')",
            [],
            |r| r.get(0),
        )?;
        let existing: bool =
            db.query_row("SELECT EXISTS(SELECT 1 FROM bodies LIMIT 1)", [], |r| {
                r.get(0)
            })?;
        if !indexed && existing {
            return Err("legacy example database needs an explicit index migration".into());
        }
        db.execute_batch("CREATE TABLE IF NOT EXISTS summaries(bits INTEGER NOT NULL, key BLOB NOT NULL, count INTEGER NOT NULL CHECK(count>0), digest BLOB NOT NULL, PRIMARY KEY(bits,key));
            CREATE TABLE IF NOT EXISTS reader_meta(id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=0));
            INSERT OR IGNORE INTO reader_meta VALUES(1,0);")?;
        Ok(Self {
            db,
            checkpoint: None,
            limits: ReaderLimits::default(),
        })
    }
    fn snapshot(&self) -> Result<SqlSnapshot<'_>> {
        // One connection/writer: this borrow prevents a local commit until
        // preparation releases its reader. Production hosts must pin their own
        // read transaction/selection when other connections can write.
        let revision =
            self.db
                .query_row("SELECT revision FROM reader_meta WHERE id=1", [], |r| {
                    r.get::<_, i64>(0)
                })?;
        Ok(SqlSnapshot {
            db: &self.db,
            revision: u64::try_from(revision)?,
        })
    }
    fn commit(
        &mut self,
        records: &[Record],
        authorize: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<bool>,
        fail: bool,
    ) -> Result<()> {
        let tx = self.db.transaction()?;
        if !authorize(&tx)? {
            return Err("authorization revoked".into());
        }
        for record in records {
            if <[u8; 32]>::from(Sha256::digest(&record.body)) != record.id {
                return Err("invalid body".into());
            }
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO bodies VALUES(?1,?2)",
                params![record.id, record.body],
            )?;
            if inserted != 0 {
                update_path(&tx, record.id)?;
                tx.execute("UPDATE reader_meta SET revision=revision+1 WHERE id=1", [])?;
            }
        }
        if fail {
            return Err("injected commit failure".into());
        }
        tx.commit()?;
        Ok(())
    }
    fn count(&self) -> Result<usize> {
        Ok(usize::try_from(self.db.query_row(
            "SELECT COUNT(*) FROM bodies",
            [],
            |r| r.get::<_, i64>(0),
        )?)?)
    }
}
struct SqlSnapshot<'a> {
    db: &'a Connection,
    revision: u64,
}
impl RangeRead for SqlSnapshot<'_> {
    type Error = rusqlite::Error;
    fn revision(&self) -> u64 {
        self.revision
    }
    fn contains(&self, id: &[u8; 32]) -> rusqlite::Result<bool> {
        self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM bodies WHERE id=?1)",
            [id],
            |r| r.get(0),
        )
    }
    fn summary(&self, prefix: Prefix) -> rusqlite::Result<Summary> {
        summary_on(self.db, prefix)
    }
}

// Leaves and real branch points only: at most 2*N-1 rows. Unary chains
// reconstruct canonical hashes in SDK code, without materializing ancestors.
fn node_on(db: &Connection, p: Prefix) -> rusqlite::Result<Option<Summary>> {
    let row: Option<(i64, [u8; 32])> = db
        .query_row(
            "SELECT count,digest FROM summaries WHERE bits=?1 AND key=?2",
            params![p.bits(), p.key()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(count, digest)| {
        let summary = Summary {
            count: usize::try_from(count).map_err(|_| rusqlite::Error::InvalidQuery)?,
            digest,
        };
        summary.lift(p, p).ok_or(rusqlite::Error::InvalidQuery)
    })
    .transpose()
}
fn summary_on(db: &Connection, p: Prefix) -> rusqlite::Result<Summary> {
    if let Some(summary) = node_on(db, p)? {
        return Ok(summary);
    }
    let mut upper = p.key();
    for bit in usize::from(p.bits())..256 {
        upper[bit / 8] |= 128 >> (bit % 8);
    }
    let edge = |order: &str| -> rusqlite::Result<Option<[u8; 32]>> {
        db.query_row(
            &format!("SELECT id FROM bodies WHERE id>=?1 AND id<=?2 ORDER BY id {order} LIMIT 1"),
            params![p.key(), upper],
            |r| r.get(0),
        )
        .optional()
    };
    let Some(first) = edge("ASC")? else {
        return Ok(Summary::empty());
    };
    let last = edge("DESC")?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    let base = Prefix::common(first, last);
    node_on(db, base)?
        .ok_or(rusqlite::Error::QueryReturnedNoRows)?
        .lift(base, p)
        .ok_or(rusqlite::Error::InvalidQuery)
}
fn put_node(db: &Connection, p: Prefix, summary: Summary) -> rusqlite::Result<()> {
    db.execute(
        "INSERT OR REPLACE INTO summaries VALUES(?1,?2,?3,?4)",
        params![
            p.bits(),
            p.key(),
            i64::try_from(summary.count).map_err(|_| rusqlite::Error::InvalidQuery)?,
            summary.digest
        ],
    )?;
    Ok(())
}
fn update_path(db: &Connection, id: [u8; 32]) -> rusqlite::Result<()> {
    let mut carry = Summary::leaf(id);
    put_node(db, Prefix::new(256, id).unwrap(), carry)?;
    for bits in (0..256).rev() {
        let p = Prefix::new(bits, id).unwrap();
        let [left, right] = p.children().unwrap();
        let (a, b) = if id[usize::from(bits) / 8] & (128 >> (bits % 8)) != 0 {
            (summary_on(db, left)?, carry)
        } else {
            (carry, summary_on(db, right)?)
        };
        carry = Summary::combine(p, a, b).ok_or(rusqlite::Error::InvalidQuery)?;
        if a.count > 0 && b.count > 0 {
            put_node(db, p, carry)?;
        }
    }
    Ok(())
}

// One bounded discovery turn followed by one atomic commit. The host keeps an
// ephemeral checkpoint across no-body discovery turns. Changed readers restart
// from committed data; errors/cancellation never replace the prior checkpoint.
// No cursor is persisted separately and no background drain is created.
async fn turn(
    store: &mut Store,
    remote: &RangeIndex,
    records: &[Record],
    mut authorize: impl FnMut() -> bool,
    mut query: impl AsyncFnMut(Prefix) -> Result<SummaryFrame>,
    fail_commit: bool,
) -> Result<TurnResult> {
    if !authorize() {
        return Err("unauthorized".into());
    }
    let prepared = {
        let snapshot = store.snapshot()?;
        let checkpoint = match store.checkpoint.as_ref() {
            Some(cp)
                if cp.valid_reader_binding(
                    &snapshot,
                    SCOPE,
                    VIEW,
                    AUTHORITY,
                    remote.revision(),
                )? =>
            {
                cp.clone()
            }
            _ => Checkpoint::for_reader(SCOPE, AUTHORITY, &snapshot, remote.revision())?,
        };
        reader::prepare(
            &checkpoint,
            &snapshot,
            ReaderSource {
                scope: SCOPE,
                authority: AUTHORITY,
                view: VIEW,
                revision: remote.revision(),
            },
            store.limits,
            &mut authorize,
            &mut query,
        )
        .await
        .map_err(|error| format!("prepare: {error:?}"))?
    };
    let mut bodies = Vec::new();
    let mut bytes = 0usize;
    for id in prepared.missing() {
        let record = records
            .iter()
            .find(|r| r.id == *id)
            .ok_or("body unavailable")?;
        bytes = bytes
            .checked_add(record.body.len())
            .filter(|size| *size <= 1024 * 1024)
            .ok_or("byte allowance exhausted")?;
        bodies.push(record.clone());
    }
    // Preparation retained no borrow of the database. Reauthorize within the
    // actual write transaction; release its continuation only after commit.
    let (next, difference) =
        prepared.commit(|_| store.commit(&bodies, |_tx| Ok(authorize()), fail_commit))?;
    store.checkpoint = Some(next);
    Ok(TurnResult {
        committed: difference.missing.len(),
        complete: difference.progress == mesh_replication::range::Progress::Complete,
    })
}
fn source() -> (RangeIndex, Vec<Record>) {
    let records: Vec<_> = [b"opaque first".as_slice(), b"opaque second".as_slice()]
        .into_iter()
        .map(|body| Record {
            id: Sha256::digest(body).into(),
            body: body.to_vec(),
        })
        .collect();
    let mut index = RangeIndex::default();
    for record in &records {
        index.insert(record.id);
    }
    (index, records)
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("records.sqlite");
    let (remote, records) = source();
    let mut store = Store::open(&path)?;
    assert_eq!(
        turn(
            &mut store,
            &remote,
            &records,
            || true,
            async |p| Ok(remote.frame(SCOPE, VIEW, p)),
            false
        )
        .await?
        .committed,
        2
    );
    drop(store);
    let mut reopened = Store::open(&path)?;
    assert_eq!(
        turn(
            &mut reopened,
            &remote,
            &records,
            || true,
            async |p| Ok(remote.frame(SCOPE, VIEW, p)),
            false
        )
        .await?
        .committed,
        0
    );
    let observed = turn(
        &mut reopened,
        &remote,
        &records,
        || true,
        async |p| Ok(remote.frame(SCOPE, VIEW, p)),
        false,
    )
    .await?;
    assert!(observed.complete);
    println!(
        "{} durable records; reopened turn commits no duplicates",
        reopened.count()?
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn rollback_revocation_and_cancellation_do_not_acknowledge_discovery() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("db");
        let (remote, records) = source();
        let mut store = Store::open(&path)?;
        assert!(
            turn(
                &mut store,
                &remote,
                &records,
                || true,
                async |p| Ok(remote.frame(SCOPE, VIEW, p)),
                true
            )
            .await
            .is_err()
        );
        assert_eq!(store.count()?, 0);
        assert_eq!(store.snapshot()?.summary(Prefix::root())?, Summary::empty());
        assert_eq!(store.snapshot()?.revision(), 0);
        let calls = std::cell::Cell::new(0);
        assert!(
            turn(
                &mut store,
                &remote,
                &records,
                || {
                    calls.set(calls.get() + 1);
                    calls.get() < 2
                },
                async |p| Ok(remote.frame(SCOPE, VIEW, p)),
                false
            )
            .await
            .is_err()
        );
        assert_eq!(store.count()?, 0);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(1),
                turn(
                    &mut store,
                    &remote,
                    &records,
                    || true,
                    async |_| std::future::pending().await,
                    false
                )
            )
            .await
            .is_err()
        );
        drop(store);
        let mut store = Store::open(&path)?;
        assert_eq!(store.count()?, 0);
        assert_eq!(
            turn(
                &mut store,
                &remote,
                &records,
                || true,
                async |p| Ok(remote.frame(SCOPE, VIEW, p)),
                false
            )
            .await?
            .committed,
            2
        );
        Ok(())
    }
    #[test]
    fn failed_sql_read_keeps_the_checkpoint_query() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = Store::open(&directory.path().join("db"))?;
        let snapshot = store.snapshot()?;
        let (remote, _) = source();
        let mut cp = Checkpoint::for_reader(SCOPE, AUTHORITY, &snapshot, remote.revision())?;
        let prior = cp.next_query();
        store.db.execute("DROP TABLE summaries", [])?;
        assert!(
            cp.accept_reader(
                &snapshot,
                SCOPE,
                VIEW,
                AUTHORITY,
                remote.revision(),
                remote.frame(SCOPE, VIEW, Prefix::root())
            )
            .is_err()
        );
        assert_eq!(cp.next_query(), prior);
        Ok(())
    }
}

#[cfg(test)]
mod transaction_authority_tests {
    use super::*;
    #[test]
    fn authorization_is_evaluated_in_the_write_transaction() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::open(&directory.path().join("db"))?;
        let (_, records) = source();
        let checked = std::cell::Cell::new(false);
        assert!(
            store
                .commit(
                    &records,
                    |tx| {
                        assert!(
                            !tx.is_autocommit(),
                            "authorization must run in the active transaction"
                        );
                        checked.set(true);
                        Ok(false)
                    },
                    false
                )
                .is_err()
        );
        assert!(checked.get());
        assert_eq!(store.count()?, 0);
        Ok(())
    }
}

#[cfg(test)]
mod maintained_index_tests {
    use super::*;
    #[test]
    fn compact_index_survives_reopen_and_snapshot_preparation_is_read_only() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("maintained.db");
        let mut store = Store::open(&path)?;
        let records: Vec<_> = (0..128u64)
            .map(|n| {
                let body = n.to_be_bytes().to_vec();
                Record {
                    id: Sha256::digest(&body).into(),
                    body,
                }
            })
            .collect();
        store.commit(&records, |_| Ok(true), false)?;
        let mut expected = RangeIndex::default();
        for r in &records {
            expected.insert(r.id);
        }
        let nodes: i64 = store
            .db
            .query_row("SELECT count(*) FROM summaries", [], |r| r.get(0))?;
        assert!(
            nodes <= 2 * records.len() as i64 - 1,
            "unary ancestors must not consume stored rows"
        );
        assert_eq!(store.snapshot()?.revision(), 128);
        store.commit(&records, |_| Ok(true), false)?;
        assert_eq!(
            store.snapshot()?.revision(),
            128,
            "idempotent writes must not change the reader version"
        );
        // The previous per-turn rebuild would fail under query_only.
        store.db.execute_batch("PRAGMA query_only=ON")?;
        {
            let reader = store.snapshot()?;
            assert_eq!(
                reader.summary(Prefix::root())?,
                expected.summary(Prefix::root())
            );
            let prefixes = [
                Prefix::root(),
                Prefix::new(1, records[0].id).unwrap(),
                Prefix::new(128, records[0].id).unwrap(),
                Prefix::new(256, records[0].id).unwrap(),
            ];
            for p in prefixes {
                assert_eq!(reader.summary(p)?, expected.summary(p));
            }
            #[cfg(feature = "test-observability")]
            mesh_replication::test_support::check_reader(
                &reader,
                &expected,
                &prefixes,
                &[records[0].id, [255; 32]],
            )
            .map_err(|e| format!("reader conformance: {e:?}"))?;
        }
        let plan:Vec<String>=store.db.prepare("EXPLAIN QUERY PLAN SELECT id FROM bodies WHERE id>=?1 AND id<=?2 ORDER BY id LIMIT 1")?
            .query_map(params![[0u8;32],[255u8;32]],|r|r.get(3))?.collect::<rusqlite::Result<_>>()?;
        assert!(
            plan.iter()
                .any(|s| s.contains("SEARCH") && s.contains("INDEX")),
            "endpoint lookup must use indexed range seeks: {plan:?}"
        );
        drop(store);
        let reopened = Store::open(&path)?;
        assert_eq!(
            reopened.snapshot()?.summary(Prefix::root())?,
            expected.summary(Prefix::root())
        );
        assert_eq!(reopened.snapshot()?.revision(), 128);
        Ok(())
    }
    #[test]
    fn unindexed_old_example_database_requires_explicit_migration() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("old.db");
        let db = Connection::open(&path)?;
        db.execute_batch("CREATE TABLE bodies(id BLOB PRIMARY KEY,body BLOB NOT NULL)")?;
        db.execute(
            "INSERT INTO bodies VALUES(?1,?2)",
            params![[7u8; 32], b"legacy".as_slice()],
        )?;
        drop(db);
        assert!(Store::open(&path).is_err());
        let db = Connection::open(path)?;
        assert_eq!(
            db.query_row("SELECT count(*) FROM bodies", [], |r| r.get::<_, i64>(0))?,
            1
        );
        Ok(())
    }
}

#[cfg(test)]
mod bounded_continuation_tests {
    use super::*;
    #[tokio::test]
    async fn no_body_turns_retain_navigation_and_never_claim_completion() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::open(&directory.path().join("bounded.db"))?;
        store.limits = ReaderLimits {
            queries: 1,
            records: 32,
        };
        let records: Vec<_> = (0..33u64)
            .map(|n| {
                let body = n.to_be_bytes().to_vec();
                Record {
                    id: Sha256::digest(&body).into(),
                    body,
                }
            })
            .collect();
        let mut remote = RangeIndex::default();
        for r in &records {
            remote.insert(r.id);
        }
        let first = turn(
            &mut store,
            &remote,
            &records,
            || true,
            async |p| Ok(remote.frame(SCOPE, VIEW, p)),
            false,
        )
        .await?;
        assert_eq!(first.committed, 0);
        assert!(
            !first.complete,
            "zero selected bodies does not mean a completed source"
        );
        let prior = store.checkpoint.as_ref().unwrap().next_query();
        assert!(
            turn(
                &mut store,
                &remote,
                &records,
                || true,
                async |p| Ok(remote.frame(SCOPE, VIEW, p)),
                true
            )
            .await
            .is_err()
        );
        assert_eq!(store.checkpoint.as_ref().unwrap().next_query(), prior);
        assert_eq!(store.count()?, 0);
        let mut done = false;
        for _ in 0..128 {
            let result = turn(
                &mut store,
                &remote,
                &records,
                || true,
                async |p| Ok(remote.frame(SCOPE, VIEW, p)),
                false,
            )
            .await?;
            if result.complete {
                done = true;
                break;
            }
        }
        assert!(done, "bounded no-body discovery must preserve its frontier");
        assert_eq!(store.count()?, 33);
        Ok(())
    }
}

#[cfg(test)]
mod body_budget_tests {
    use super::*;
    #[tokio::test]
    async fn oversized_body_cannot_commit_or_advance_discovery() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::open(&directory.path().join("oversized.db"))?;
        let body = vec![0; 1024 * 1024 + 1];
        let record = Record {
            id: Sha256::digest(&body).into(),
            body,
        };
        let mut remote = RangeIndex::default();
        remote.insert(record.id);
        assert!(
            turn(
                &mut store,
                &remote,
                &[record],
                || true,
                async |p| Ok(remote.frame(SCOPE, VIEW, p)),
                false
            )
            .await
            .is_err()
        );
        assert_eq!(store.count()?, 0);
        assert!(store.checkpoint.is_none());
        assert_eq!(store.snapshot()?.revision(), 0);
        Ok(())
    }
}
