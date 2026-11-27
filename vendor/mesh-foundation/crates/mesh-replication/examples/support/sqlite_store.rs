use mesh_replication::{
    pull::Record,
    range::{Checkpoint, Prefix, RangeRead, Summary},
    reader::ReaderLimits,
};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{error::Error, path::Path};

pub(super) type Result<T> = std::result::Result<T, Box<dyn Error>>;
pub(super) const SCOPE: [u8; 32] = [1; 32];
pub(super) const AUTHORITY: [u8; 32] = [2; 32];

pub(super) struct Store {
    pub(super) db: Connection,
    pub(super) checkpoint: Option<Checkpoint>,
    pub(super) limits: ReaderLimits,
}
impl Store {
    pub(super) fn open(path: &Path) -> Result<Self> {
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
    pub(super) fn snapshot(&self) -> Result<SqlSnapshot<'_>> {
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
    pub(super) fn commit(
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
    pub(super) fn count(&self) -> Result<usize> {
        Ok(usize::try_from(self.db.query_row(
            "SELECT COUNT(*) FROM bodies",
            [],
            |r| r.get::<_, i64>(0),
        )?)?)
    }
}
pub(super) struct SqlSnapshot<'a> {
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
