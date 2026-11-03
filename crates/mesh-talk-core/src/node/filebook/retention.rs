//! Current file presentation metadata and newest-offer order. Both are derived
//! from accepted FILES rows in their installation transaction; deletion cascades.
use super::indexed::{IndexedFileBook, blob, invalid};
use crate::{
    eventlog::{ConversationId, LogError},
    file::{AnyManifest, FileKind},
};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

const SCHEMA:&str="
CREATE TABLE file_book_offer_retention(account BLOB NOT NULL CHECK(length(account)=32),ordinal INTEGER NOT NULL,file BLOB NOT NULL CHECK(length(file)=32),clock BLOB NOT NULL CHECK(length(clock)=8),chunks INTEGER NOT NULL CHECK(typeof(chunks)='integer' AND chunks BETWEEN 1 AND 4294967295),mode INTEGER NOT NULL CHECK(mode BETWEEN 0 AND 2),PRIMARY KEY(account,ordinal),FOREIGN KEY(account,ordinal) REFERENCES file_book_offers(account,ordinal) ON DELETE CASCADE) WITHOUT ROWID;
CREATE INDEX file_book_retention_first ON file_book_offer_retention(account,file,ordinal);
CREATE INDEX file_book_retention_latest ON file_book_offer_retention(account,file,clock DESC,ordinal DESC);
CREATE TABLE file_book_retention(account BLOB NOT NULL CHECK(length(account)=32),file BLOB NOT NULL CHECK(length(file)=32),newest BLOB NOT NULL CHECK(length(newest)=8),first_ordinal INTEGER NOT NULL,chunks INTEGER NOT NULL CHECK(typeof(chunks)='integer' AND chunks BETWEEN 1 AND 4294967295),mode INTEGER NOT NULL CHECK(mode BETWEEN 0 AND 2),PRIMARY KEY(account,file),FOREIGN KEY(account,file) REFERENCES file_book_bindings(account,file)) WITHOUT ROWID;
CREATE INDEX file_book_retention_order ON file_book_retention(account,newest DESC,file DESC);
";
const TRIGGERS: &[(&str, &str)] = &[
    (
        "file_book_retention_added",
        "CREATE TRIGGER file_book_retention_added AFTER INSERT ON file_book_offer_retention BEGIN INSERT INTO file_book_retention VALUES(NEW.account,NEW.file,NEW.clock,NEW.ordinal,NEW.chunks,NEW.mode) ON CONFLICT(account,file) DO UPDATE SET newest=max(newest,excluded.newest),chunks=CASE WHEN excluded.first_ordinal<first_ordinal THEN excluded.chunks ELSE chunks END,mode=CASE WHEN excluded.first_ordinal<first_ordinal THEN excluded.mode ELSE mode END,first_ordinal=min(first_ordinal,excluded.first_ordinal); END",
    ),
    (
        "file_book_retention_removed",
        "CREATE TRIGGER file_book_retention_removed AFTER DELETE ON file_book_offer_retention BEGIN DELETE FROM file_book_retention WHERE account=OLD.account AND file=OLD.file; INSERT INTO file_book_retention SELECT p.account,p.file,(SELECT clock FROM file_book_offer_retention WHERE account=OLD.account AND file=OLD.file ORDER BY clock DESC,ordinal DESC LIMIT 1),p.ordinal,p.chunks,p.mode FROM file_book_offer_retention p WHERE p.account=OLD.account AND p.file=OLD.file ORDER BY p.ordinal LIMIT 1; END",
    ),
];

pub(super) fn initialize_on(c: &Connection) -> Result<(), LogError> {
    let present: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='file_book_retention')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        c.execute_batch(SCHEMA)?;
        for (_, sql) in TRIGGERS {
            c.execute_batch(sql)?;
        }
        // One-time upgrade of accepted current facts. Only 32 fixed keys and
        // one pre-admitted manifest body are resident; no all-history sort.
        let mut after: Option<([u8; 32], i64)> = None;
        loop {
            let keys = {
                let mut statement=c.prepare("SELECT account,ordinal FROM file_book_offers WHERE (account,ordinal)>(?1,?2) ORDER BY account,ordinal LIMIT 32")?;
                let mut rows = statement.query(params![
                    after.map_or_else(Vec::new, |a| a.0.to_vec()),
                    after.map_or(0, |a| a.1)
                ])?;
                let mut keys = Vec::new();
                while let Some(row) = rows.next()? {
                    keys.push((blob::<32>(row, 0)?, row.get::<_, i64>(1)?));
                }
                keys
            };
            if keys.is_empty() {
                break;
            }
            for (account, ordinal) in keys {
                let (clock,expected,declared,physical):([u8;8],[u8;32],i64,i64)=c.query_row("SELECT r.clock,r.plaintext_sha256,r.body_bytes,length(b.plaintext) FROM transcript_rows r JOIN transcript_bodies b USING(ordinal) WHERE r.ordinal=?1",[ordinal],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
                if declared <= 0
                    || declared != physical
                    || declared as u64 > crate::node::transcript_repository::MAX_PAGE_BYTES as u64
                {
                    return Err(invalid("invalid file retention body budget"));
                }
                let bytes: Vec<u8> = c.query_row(
                    "SELECT plaintext FROM transcript_bodies WHERE ordinal=?1",
                    [ordinal],
                    |r| r.get(0),
                )?;
                if <[u8; 32]>::from(Sha256::digest(&bytes)) != expected {
                    return Err(invalid("file retention source digest mismatch"));
                }
                let manifest = crate::node::files::validated_manifest(&bytes)
                    .ok_or_else(|| invalid("invalid file retention manifest"))?;
                let (file,binding):([u8;32],[u8;32])=c.query_row("SELECT o.file,b.digest FROM file_book_offers o JOIN file_book_bindings b ON b.account=o.account AND b.file=o.file WHERE o.account=?1 AND o.ordinal=?2",params![account.as_slice(),ordinal],|r|Ok((r.get(0)?,r.get(1)?)))?;
                if manifest.file_conv().as_bytes() != &file
                    || crate::node::files::file_transfer_fingerprint(&manifest) != Some(binding)
                {
                    return Err(invalid("file retention binding disagrees with body"));
                }
                record_on(c, &account, ordinal, u64::from_be_bytes(clock), &manifest)?;
                after = Some((account, ordinal));
            }
        }
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
    for (name, expected) in TRIGGERS {
        let actual: Option<String> = c
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='trigger' AND name=?1",
                [name],
                |r| r.get(0),
            )
            .optional()?;
        if actual.as_deref() != Some(*expected) {
            return Err(invalid("incompatible file retention projection"));
        }
    }
    Ok(())
}

pub(super) fn record_on(
    c: &Connection,
    account: &[u8; 32],
    ordinal: i64,
    clock: u64,
    manifest: &AnyManifest,
) -> Result<(), LogError> {
    let mode = if manifest.kind() == Some(FileKind::File) {
        1
    } else if crate::node::media_store::manifest_is_media(manifest) {
        2
    } else {
        0
    };
    c.execute(
        "INSERT OR IGNORE INTO file_book_offer_retention VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            account.as_slice(),
            ordinal,
            manifest.file_conv().as_bytes().as_slice(),
            clock.to_be_bytes().as_slice(),
            i64::from(manifest.chunk_count()),
            mode
        ],
    )?;
    Ok(())
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FileRetention {
    pub file: ConversationId,
    pub newest: u64,
    pub chunks: u32,
    /// 0: no replica; 1: ordinary file; 2: media.
    pub mode: u8,
}

fn read(row: &rusqlite::Row<'_>) -> Result<FileRetention, LogError> {
    let chunks: i64 = row.get(2)?;
    let mode: i64 = row.get(3)?;
    Ok(FileRetention {
        file: ConversationId::new(blob(row, 0)?),
        newest: u64::from_be_bytes(blob(row, 1)?),
        chunks: u32::try_from(chunks)
            .ok()
            .filter(|count| *count > 0)
            .ok_or_else(|| invalid("invalid retention chunk count"))?,
        mode: u8::try_from(mode)
            .ok()
            .filter(|m| *m <= 2)
            .ok_or_else(|| invalid("invalid retention mode"))?,
    })
}

impl IndexedFileBook {
    pub(crate) fn retention_for(
        &self,
        file: ConversationId,
    ) -> Result<Option<FileRetention>, LogError> {
        self.profile.database().read(|c|{
            self.check(c)?;
            let mut statement=c.prepare_cached("SELECT file,newest,chunks,mode FROM file_book_retention WHERE account=?1 AND file=?2")?;
            let mut rows=statement.query(params![self.account.as_slice(),file.as_bytes().as_slice()])?;
            rows.next()?.map(read).transpose()
        })
    }
    pub(crate) fn retention_page(
        &self,
        after: Option<(u64, ConversationId)>,
    ) -> Result<Vec<FileRetention>, LogError> {
        self.profile.database().read(|c|{
            self.check(c)?;
            let condition=if after.is_some(){" AND (newest,file)<(?2,?3)"}else{""};
            let mut statement=c.prepare_cached(&format!("SELECT file,newest,chunks,mode FROM file_book_retention WHERE account=?1{condition} ORDER BY newest DESC,file DESC LIMIT 32"))?;
            statement.raw_bind_parameter(1,self.account.as_slice())?;
            if let Some((clock,file))=after {statement.raw_bind_parameter(2,clock.to_be_bytes().as_slice())?;statement.raw_bind_parameter(3,file.as_bytes().as_slice())?;}
            let mut rows=statement.raw_query();
            let mut result=Vec::new();
            while let Some(row)=rows.next()? {result.push(read(row)?);}
            Ok(result)
        })
    }
}
