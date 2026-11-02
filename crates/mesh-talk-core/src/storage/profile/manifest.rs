//! Authenticated role, source-layout and publication authority. Locators are
//! root-relative OS names; moving a closed profile does not change its identity.
use super::context::{RELAY_ARRIVALS, RELAY_EVENTS};
use crate::{eventlog::LogError, storage::database::EncryptedDatabase};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    ffi::OsString,
    path::{Component, Path, PathBuf},
};

pub(super) const PREPARED: i64 = 0;
pub(super) const FROZEN: i64 = 1;
pub(super) const IMPORTING: i64 = 2;
pub(super) const VERIFIED: i64 = 3;
pub(super) const PUBLISHED: i64 = 4;
pub(super) const SCHEMA: &str = "
CREATE TABLE relay_publication(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=1), role TEXT NOT NULL CHECK(role='relay-v1'), staging_pending INTEGER NOT NULL DEFAULT 1 CHECK(staging_pending IN (0,1)));
CREATE TABLE relay_sources(source INTEGER PRIMARY KEY CHECK(source IN (0,1)), domain BLOB NOT NULL UNIQUE CHECK(length(domain)=16), source_uuid BLOB NOT NULL UNIQUE CHECK(length(source_uuid)=16), locator BLOB NOT NULL UNIQUE, archive BLOB, work BLOB, raw_size INTEGER NOT NULL CHECK(raw_size>=0), raw_hash BLOB CHECK(raw_hash IS NULL OR length(raw_hash)=32), expected_stub BLOB NOT NULL CHECK(length(expected_stub)=54), phase INTEGER NOT NULL CHECK(phase BETWEEN 0 AND 4), authenticated_records INTEGER NOT NULL CHECK(authenticated_records>=0), imported_records INTEGER NOT NULL DEFAULT 0 CHECK(imported_records>=0), unique_records INTEGER NOT NULL DEFAULT 0 CHECK(unique_records>=0), cleanup_pending INTEGER NOT NULL DEFAULT 0 CHECK(cleanup_pending IN (0,1)), cleanup_error TEXT);
";

pub(crate) fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}
#[derive(Debug)]
pub(super) struct Source {
    pub source: i64,
    pub locator: PathBuf,
    pub archive: Option<PathBuf>,
    pub work: Option<PathBuf>,
    pub raw_size: u64,
    pub raw_hash: Option<Vec<u8>>,
    pub stub: Vec<u8>,
    pub phase: i64,
    pub authenticated_records: i64,
}

pub(crate) fn encode(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect()
    }
    #[cfg(not(any(unix, windows)))]
    {
        path.as_os_str().as_encoded_bytes().to_vec()
    }
}
pub(super) fn decode(bytes: Vec<u8>) -> Result<PathBuf, LogError> {
    #[cfg(unix)]
    let value = {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(bytes)
    };
    #[cfg(windows)]
    let value = {
        use std::os::windows::ffi::OsStringExt;
        let (pairs, remainder) = bytes.as_chunks::<2>();
        if !remainder.is_empty() {
            return Err(invalid("invalid Windows source locator"));
        }
        OsString::from_wide(
            &pairs
                .iter()
                .copied()
                .map(u16::from_le_bytes)
                .collect::<Vec<_>>(),
        )
    };
    #[cfg(not(any(unix, windows)))]
    let value = OsString::from(
        String::from_utf8(bytes).map_err(|_| invalid("unsupported source locator"))?,
    );
    let path = PathBuf::from(value);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(invalid("source locator must remain inside profile root"));
    }
    Ok(path)
}
pub(super) fn sources(db: &EncryptedDatabase) -> Result<Vec<Source>, LogError> {
    db.read(|c| {
        let mut stmt=c.prepare("SELECT source,locator,archive,work,raw_size,raw_hash,expected_stub,phase,authenticated_records FROM relay_sources ORDER BY source LIMIT 3")?;
        let mut rows=stmt.query([])?; let mut result=Vec::with_capacity(2);
        while let Some(r)=rows.next()? {
            let size:i64=r.get(4)?;
            result.push(Source {source:r.get(0)?,locator:decode(r.get(1)?)?,archive:r.get::<_,Option<Vec<u8>>>(2)?.map(decode).transpose()?,work:r.get::<_,Option<Vec<u8>>>(3)?.map(decode).transpose()?,raw_size:u64::try_from(size).map_err(|_|invalid("invalid raw size"))?,raw_hash:r.get(5)?,stub:r.get(6)?,phase:r.get(7)?,authenticated_records:r.get(8)?});
        }
        if result.len()!=2 || result[0].source!=0 || result[1].source!=1 { return Err(invalid("missing mandatory relay source")); }
        Ok(result)
    })
}
pub(super) fn create(db: &EncryptedDatabase) -> Result<(), LogError> {
    db.transaction(|tx| {
        tx.execute_batch(SCHEMA)?;
        tx.execute(
            "INSERT INTO relay_publication(singleton,version,role) VALUES(1,1,'relay-v1')",
            [],
        )?;
        Ok(())
    })
}
pub(crate) fn validate_ddl(c: &Connection, schema: &str) -> Result<(), LogError> {
    for statement in schema.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let object = statement
            .strip_prefix("CREATE TABLE ")
            .map(|s| (s, "table"))
            .or_else(|| {
                statement
                    .strip_prefix("CREATE INDEX ")
                    .map(|s| (s, "index"))
            })
            .or_else(|| {
                statement
                    .strip_prefix("CREATE UNIQUE INDEX ")
                    .map(|s| (s, "index"))
            })
            .ok_or_else(|| invalid("invalid owned schema declaration"))?;
        let name = object
            .0
            .split(['(', ' '])
            .next()
            .ok_or_else(|| invalid("invalid schema name"))?;
        let actual: Option<String> = c
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name=?1 AND type=?2",
                params![name, object.1],
                |r| r.get(0),
            )
            .optional()?;
        if actual.as_deref().map(str::trim) != Some(statement) {
            return Err(invalid("incompatible relay publication schema"));
        }
    }
    Ok(())
}
pub(super) fn validate(db: &EncryptedDatabase, paths: &[PathBuf; 2]) -> Result<(), LogError> {
    db.read(|c| {
        validate_ddl(c,SCHEMA)?;
        let (version,role):(i64,String)=c.query_row("SELECT version,role FROM relay_publication WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if version!=1 || role!="relay-v1" { return Err(invalid("incompatible profile role")); }
        for (index,domain) in [RELAY_EVENTS.0, RELAY_ARRIVALS].iter().enumerate() {
            let (locator,actual,source_uuid,stub):(Vec<u8>,Vec<u8>,Vec<u8>,Vec<u8>)=c.query_row("SELECT locator,domain,source_uuid,expected_stub FROM relay_sources WHERE source=?1",[index as i64],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            let store_uuid:Vec<u8>=c.query_row("SELECT store_uuid FROM mesh_database_metadata WHERE singleton=1",[],|r|r.get(0))?;
            let mut expected=b"MTSDB1".to_vec();expected.extend_from_slice(&store_uuid);expected.extend_from_slice(&source_uuid);expected.extend_from_slice(domain);
            if stub!=expected {return Err(invalid("source barrier is not bound to the authenticated manifest"));}
            if locator!=encode(&paths[index]) || actual!=domain { return Err(invalid("incompatible relay source layout")); }
        }
        Ok(())
    })?;
    sources(db)?;
    Ok(())
}
pub(super) fn require_published(db: &EncryptedDatabase) -> Result<(), LogError> {
    db.read(|c| {
        let count: i64 = c.query_row(
            "SELECT count(*) FROM relay_sources WHERE phase=4",
            [],
            |r| r.get(0),
        )?;
        if count != 2 {
            return Err(invalid("relay sources are not published"));
        }
        Ok(())
    })
}
pub(super) fn phase(db: &EncryptedDatabase, source: i64, phase: i64) -> Result<(), LogError> {
    db.transaction(|tx| {
        tx.execute(
            "UPDATE relay_sources SET phase=?1 WHERE source=?2",
            params![phase, source],
        )?;
        Ok(())
    })
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStringExt;

    #[test]
    fn windows_locator_preserves_non_bmp_and_unpaired_utf16() {
        // A non-BMP character followed by separated lone high/low surrogates.
        let path = PathBuf::from(OsString::from_wide(&[
            0x0061, 0xd83d, 0xde80, 0x0062, 0xd800, 0x0063, 0xdc00,
        ]));
        let bytes = vec![
            0x61, 0x00, 0x3d, 0xd8, 0x80, 0xde, 0x62, 0x00, 0x00, 0xd8, 0x63, 0x00, 0x00, 0xdc,
        ];
        assert_eq!(encode(&path), bytes);
        assert_eq!(decode(bytes).unwrap(), path);
    }

    #[test]
    fn windows_locator_rejects_trailing_partial_utf16_unit() {
        assert!(matches!(
            decode(vec![0x61, 0x00, 0x62]),
            Err(LogError::CorruptFile(_))
        ));
    }
}

#[cfg(test)]
mod ddl_tests {
    use super::*;
    #[test]
    fn unique_index_authority_is_checked_exactly_and_never_skipped() {
        const EXPECTED: &str = "CREATE TABLE pending(domain BLOB,active INTEGER);CREATE UNIQUE INDEX single_pending ON pending(domain) WHERE active=1;";
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(EXPECTED).unwrap();
        validate_ddl(&c, EXPECTED).unwrap();
        c.execute_batch("DROP INDEX single_pending").unwrap();
        assert!(validate_ddl(&c, EXPECTED).is_err());
        c.execute_batch("CREATE INDEX single_pending ON pending(domain) WHERE active=1")
            .unwrap();
        assert!(
            validate_ddl(&c, EXPECTED).is_err(),
            "nonunique replacement cannot satisfy unique authority"
        );
    }
}
