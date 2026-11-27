//! SQLite composition, not a production database schema or network protocol.
//! One authorized scope and one connection/writer. Commits maintain a compact
//! summary index atomically with bodies. Snapshot
//! creation reads only its revision; range reads use indexed endpoint seeks. Real hosts
//! authenticate the peer and verify application signatures in the transaction.
#[path = "support/sqlite_store.rs"]
mod sqlite_store;
use mesh_replication::{
    pull::Record,
    range::{Checkpoint, Prefix, RangeIndex, SummaryFrame},
    reader::{self, ReaderSource},
};
#[cfg(test)]
use mesh_replication::{
    range::{RangeRead, Summary},
    reader::ReaderLimits,
};
#[cfg(test)]
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use sqlite_store::*;
const VIEW: [u8; 32] = [3; 32];
struct TurnResult {
    committed: usize,
    complete: bool,
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
    mut query: impl AsyncFnMut(Prefix) -> std::io::Result<SummaryFrame>,
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
        .await?
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
