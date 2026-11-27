//! SQLite composition, not a production database schema or network protocol.
//! Setup materializes a small authorized snapshot once; summary queries never
//! rebuild it. Large hosts must budget/index preparation separately. Real hosts
//! authenticate the peer and verify application signatures in the transaction.
use mesh_replication::{
    pull::Record,
    range::{Checkpoint, Prefix, RangeIndex, RangeRead, Summary, SummaryFrame},
};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, error::Error, path::Path};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const SCOPE: [u8; 32] = [1; 32];
const AUTHORITY: [u8; 32] = [2; 32];
const VIEW: [u8; 32] = [3; 32];

struct Store {
    db: Connection,
}
impl Store {
    fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS bodies(id BLOB PRIMARY KEY, body BLOB NOT NULL);
            CREATE TEMP TABLE summaries(bits INTEGER, key BLOB, count INTEGER, digest BLOB, PRIMARY KEY(bits,key));")?;
        Ok(Self { db })
    }
    fn snapshot(&self) -> Result<SqlSnapshot<'_>> {
        let ids = self
            .db
            .prepare("SELECT id FROM bodies ORDER BY id")?
            .query_map([], |r| r.get::<_, [u8; 32]>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut index = RangeIndex::default();
        let mut prefixes = BTreeSet::from([Prefix::root()]);
        for id in &ids {
            index.insert(*id);
            for bits in 1..=256 {
                prefixes.insert(Prefix::new(bits, *id).unwrap());
            }
        }
        self.db.execute("DELETE FROM summaries", [])?;
        for prefix in prefixes {
            let summary = index.summary(prefix);
            self.db.execute(
                "INSERT INTO summaries VALUES(?1,?2,?3,?4)",
                params![
                    prefix.bits(),
                    prefix.key(),
                    summary.count as i64,
                    summary.digest
                ],
            )?;
        }
        // Borrowing the store for this snapshot prevents local writes during
        // discovery. The example has one connection/writer, no concurrent owner.
        Ok(SqlSnapshot {
            db: &self.db,
            revision: ids.len() as u64,
        })
    }
    fn commit(&mut self, records: &[Record], allowed: bool, fail: bool) -> Result<()> {
        let tx = self.db.transaction()?;
        if !allowed {
            return Err("authorization revoked".into());
        }
        for record in records {
            if <[u8; 32]>::from(Sha256::digest(&record.body)) != record.id {
                return Err("invalid body".into());
            }
            tx.execute(
                "INSERT OR IGNORE INTO bodies VALUES(?1,?2)",
                params![record.id, record.body],
            )?;
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
        Ok(self
            .db
            .query_row(
                "SELECT count,digest FROM summaries WHERE bits=?1 AND key=?2",
                params![prefix.bits(), prefix.key()],
                |r| {
                    Ok(Summary {
                        count: usize::try_from(r.get::<_, i64>(0)?).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Integer,
                                Box::new(error),
                            )
                        })?,
                        digest: r.get(1)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_else(Summary::empty))
    }
}

// One bounded discovery turn followed by one atomic commit. Checkpoints are
// ephemeral: failure/cancellation discards them; restart reads committed bodies.
// No cursor is saved separately from data and no historical drain is scheduled.
async fn turn(
    store: &mut Store,
    remote: &RangeIndex,
    records: &[Record],
    mut authorize: impl FnMut() -> bool,
    mut query: impl AsyncFnMut(Prefix) -> Result<SummaryFrame>,
    fail_commit: bool,
) -> Result<usize> {
    if !authorize() {
        return Err("unauthorized".into());
    }
    let snapshot = store.snapshot()?;
    let mut checkpoint = Checkpoint::for_reader(SCOPE, AUTHORITY, &snapshot, remote.revision())?;
    let mut missing = Vec::new();
    for _ in 0..8 {
        let Some(prefix) = checkpoint.next_query() else {
            break;
        };
        let frame = query(prefix).await?;
        if !authorize() {
            return Err("authorization revoked".into());
        }
        missing.extend(
            checkpoint
                .accept_reader(&snapshot, SCOPE, VIEW, AUTHORITY, remote.revision(), frame)?
                .map_err(|error| format!("invalid range: {error:?}"))?,
        );
    }
    if missing.len() > 200 {
        return Err("record allowance exhausted".into());
    }
    let bodies: Vec<_> = missing
        .iter()
        .map(|id| {
            records
                .iter()
                .find(|r| r.id == *id)
                .cloned()
                .ok_or("body unavailable")
        })
        .collect::<std::result::Result<_, _>>()?;
    if bodies.iter().map(|r| r.body.len()).sum::<usize>() > 1024 * 1024 {
        return Err("byte allowance exhausted".into());
    }
    store.commit(&bodies, authorize(), fail_commit)?;
    Ok(bodies.len())
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
        .await?,
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
        .await?,
        0
    );
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
            .await?,
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
