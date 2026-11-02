//! Source identity selects a registry row; publication and barrier recovery remain
//! in the single parent state machine. Unscoped source keys are unchanged.
use super::*;
const SCHEMA: &str = "CREATE TABLE account_history_sources(kind INTEGER NOT NULL CHECK(kind=15),account BLOB NOT NULL CHECK(length(account)=32),locator BLOB NOT NULL UNIQUE,stub BLOB NOT NULL CHECK(length(stub)=54),raw_size INTEGER NOT NULL CHECK(raw_size>=0),raw_hash BLOB NOT NULL CHECK(length(raw_hash)=32),published INTEGER NOT NULL CHECK(published IN(0,1)),PRIMARY KEY(kind,account)) WITHOUT ROWID;";
const AUXILIARY_SCHEMA: &str = "CREATE TABLE auxiliary_history_sources(kind INTEGER PRIMARY KEY CHECK(kind BETWEEN 16 AND 18),locator BLOB NOT NULL UNIQUE,stub BLOB NOT NULL CHECK(length(stub)=54),raw_size INTEGER NOT NULL CHECK(raw_size>=0),raw_hash BLOB NOT NULL CHECK(length(raw_hash)=32),published INTEGER NOT NULL CHECK(published IN(0,1)));";

#[derive(Clone, Copy)]
pub(super) struct SourceIdentity {
    pub kind: i64,
    account: Option<[u8; 32]>,
}
impl SourceIdentity {
    pub fn new(kind: i64, account: Option<[u8; 32]>) -> Result<Self, LogError> {
        domain(kind)?;
        if (kind == AUTHORED_REACTIONS) != account.is_some() {
            return Err(invalid(
                "history source account scope is required only for authored reactions",
            ));
        }
        Ok(Self { kind, account })
    }
    pub fn table(self) -> &'static str {
        if self.account.is_some() {
            "account_history_sources"
        } else if self.kind >= CHANNEL_CUSTODY {
            "auxiliary_history_sources"
        } else {
            "transcript_sources"
        }
    }
    fn predicate(self) -> &'static str {
        if self.account.is_some() {
            "kind=?1 AND account=?2"
        } else {
            "kind=?1 AND ?2 IS NULL"
        }
    }
    pub fn initialize(self, profile: &ProfileStorage) -> Result<(), LogError> {
        initialize(profile)?;
        if self.table() == "transcript_sources" {
            return Ok(());
        }
        let schema = if self.account.is_some() {
            SCHEMA
        } else {
            AUXILIARY_SCHEMA
        };
        profile.database().transaction(|c| {
            let present: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)",
                [self.table()],
                |r| r.get(0),
            )?;
            if !present {
                c.execute_batch(schema)?;
            }
            manifest::validate_ddl(c, schema)
        })
    }
    pub fn load(self, c: &Connection, locator: &[u8]) -> Result<Option<Source>, LogError> {
        let sql = format!(
            "SELECT locator,stub,raw_size,raw_hash,published FROM {} WHERE {}",
            self.table(),
            self.predicate()
        );
        let mut statement = c.prepare(&sql)?;
        let mut rows = statement.query(params![
            self.kind,
            self.account.as_ref().map(|a| a.as_slice())
        ])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let actual: Vec<u8> = row.get(0)?;
        if actual != locator {
            return Err(invalid("transcript source locator changed"));
        }
        let source = Source {
            stub: row.get(1)?,
            raw_size: u64::try_from(row.get::<_, i64>(2)?)
                .map_err(|_| invalid("invalid transcript source length"))?,
            raw_hash: row.get(3)?,
            published: row.get(4)?,
        };
        let uuid: Vec<u8> = c.query_row(
            "SELECT store_uuid FROM mesh_database_metadata WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if source.stub.len() != 54
            || &source.stub[..6] != b"MTSQL2"
            || source.stub[6..22] != uuid
            || source.stub[22..38] != domain(self.kind)?
            || source.raw_hash.len() != 32
        {
            return Err(invalid("invalid transcript publication identity"));
        }
        Ok(Some(source))
    }
    pub fn insert(self, c: &Connection, locator: &[u8], stub: &[u8]) -> Result<(), LogError> {
        if let Some(account) = self.account {
            c.execute("INSERT INTO account_history_sources(kind,account,locator,stub,raw_size,raw_hash,published) VALUES(?1,?2,?3,?4,0,zeroblob(32),0)",params![self.kind,account.as_slice(),locator,stub])?;
        } else {
            c.execute(&format!("INSERT INTO {}(kind,locator,stub,raw_size,raw_hash,published) VALUES(?1,?2,?3,0,zeroblob(32),0)",self.table()),params![self.kind,locator,stub])?;
        }
        Ok(())
    }
    pub fn record_raw(self, c: &Connection, length: u64, hash: &[u8]) -> Result<(), LogError> {
        let sql = format!(
            "UPDATE {} SET raw_size=?3,raw_hash=?4 WHERE {}",
            self.table(),
            self.predicate()
        );
        c.execute(
            &sql,
            params![
                self.kind,
                self.account.as_ref().map(|a| a.as_slice()),
                i64::try_from(length)
                    .map_err(|_| invalid("legacy transcript source is too large"))?,
                hash
            ],
        )?;
        Ok(())
    }
    pub fn mark_published(self, c: &Connection) -> Result<(), LogError> {
        let sql = format!(
            "UPDATE {} SET published=1 WHERE {} AND published=0",
            self.table(),
            self.predicate()
        );
        c.execute(
            &sql,
            params![self.kind, self.account.as_ref().map(|a| a.as_slice())],
        )?;
        Ok(())
    }
}
