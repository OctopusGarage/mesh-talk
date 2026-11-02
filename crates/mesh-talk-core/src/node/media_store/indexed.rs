//! Derived filename inventory on the existing profile connection. The encrypted
//! file remains the byte authority; stale locators never imply readable bytes.
use super::{ConversationId, invalid};
use crate::{
    eventlog::LogError,
    storage::profile::{context::ProfileStorage, manifest},
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, sync::Arc};
const SCHEMA:&str="
CREATE TABLE media_namespaces(id INTEGER PRIMARY KEY,locator TEXT NOT NULL,owner TEXT NOT NULL,imported INTEGER NOT NULL CHECK(imported IN(0,1)),revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=0),entries INTEGER NOT NULL CHECK(typeof(entries)='integer' AND entries>=0),gc_active INTEGER NOT NULL CHECK(gc_active IN(0,1)),gc_through INTEGER NOT NULL CHECK(gc_through>=0),gc_requested INTEGER NOT NULL CHECK(gc_requested>=0),gc_after TEXT,UNIQUE(locator,owner));
CREATE TABLE media_files(namespace INTEGER NOT NULL REFERENCES media_namespaces(id),name TEXT NOT NULL CHECK(length(CAST(name AS BLOB)) BETWEEN 1 AND 255),file BLOB NOT NULL CHECK(length(file)=32),legacy INTEGER NOT NULL CHECK(legacy IN(0,1)),partial INTEGER NOT NULL CHECK(partial IN(0,1)),revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0),PRIMARY KEY(namespace,name)) WITHOUT ROWID;
CREATE INDEX media_file_ids ON media_files(namespace,file,name);
CREATE INDEX media_legacy_files ON media_files(namespace,name) WHERE legacy=1;
";
fn io(error: LogError) -> std::io::Error {
    std::io::Error::other(error)
}
#[derive(Clone, Debug)]
pub(super) struct MediaEntry {
    pub name: String,
    pub file: ConversationId,
    pub legacy: bool,
    pub partial: bool,
    pub revision: i64,
}
fn entry(row: &rusqlite::Row<'_>) -> Result<MediaEntry, LogError> {
    let file: Vec<u8> = row.get(1)?;
    let value = MediaEntry {
        name: row.get(0)?,
        file: ConversationId::new(
            file.try_into()
                .map_err(|_| LogError::CorruptFile("invalid media locator ID".into()))?,
        ),
        legacy: row.get(2)?,
        partial: row.get(3)?,
        revision: row.get(4)?,
    };
    validate_locator(&value.name, value.file, value.partial)?;
    Ok(value)
}
fn validate_locator(name: &str, file: ConversationId, partial: bool) -> Result<(), LogError> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
        || name.contains('/')
        || name.contains('\\')
        || (partial && !name.ends_with(".part"))
    {
        return Err(LogError::CorruptFile("invalid media filename".into()));
    }
    let original = if partial {
        &name[..name.len() - 5]
    } else {
        name
    };
    let stem = Path::new(original)
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| LogError::CorruptFile("invalid media filename".into()))?;
    let mut bytes = [0; 32];
    if stem.len() != 64
        || hex::decode_to_slice(stem, &mut bytes).is_err()
        || bytes != *file.as_bytes()
    {
        return Err(LogError::CorruptFile(
            "media filename identity mismatch".into(),
        ));
    }
    Ok(())
}
pub(super) struct MediaIndex {
    profile: Arc<ProfileStorage>,
    namespace: i64,
    limit: Option<usize>,
}
impl MediaIndex {
    pub(super) fn open(
        profile: Arc<ProfileStorage>,
        dir: &Path,
        owner: &str,
        limit: Option<usize>,
    ) -> std::io::Result<Self> {
        let canonical = std::fs::canonicalize(dir)?;
        let relative = canonical
            .strip_prefix(profile.root())
            .map_err(|_| invalid("media directory is outside its profile"))?;
        let locator = relative
            .to_str()
            .ok_or_else(|| invalid("media directory is not UTF-8"))?;
        let namespace=profile.database().transaction(|c|{
   let present:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='media_namespaces')",[],|r|r.get(0))?;
   if !present{c.execute_batch(SCHEMA)?;}
   manifest::validate_ddl(c,SCHEMA)?;
   c.execute("INSERT OR IGNORE INTO media_namespaces(locator,owner,imported,revision,entries,gc_active,gc_through,gc_requested) VALUES(?1,?2,0,0,0,0,0,0)",params![locator,owner])?;
   Ok(c.query_row("SELECT id FROM media_namespaces WHERE locator=?1 AND owner=?2",params![locator,owner],|r|r.get(0))?)
  }).map_err(io)?;
        Ok(Self {
            profile,
            namespace,
            limit,
        })
    }
    pub(super) fn imported(&self) -> std::io::Result<bool> {
        self.profile
            .database()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT imported FROM media_namespaces WHERE id=?1",
                    [self.namespace],
                    |r| r.get(0),
                )?)
            })
            .map_err(io)
    }
    pub(super) fn finish_import(&self) -> std::io::Result<()> {
        self.profile
            .database()
            .transaction(|c| {
                c.execute(
                    "UPDATE media_namespaces SET imported=1 WHERE id=?1",
                    [self.namespace],
                )?;
                Ok(())
            })
            .map_err(io)
    }
    pub(super) fn revision(&self) -> std::io::Result<i64> {
        self.profile
            .database()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT revision FROM media_namespaces WHERE id=?1",
                    [self.namespace],
                    |r| r.get(0),
                )?)
            })
            .map_err(io)
    }
    fn increment_on(&self, c: &Connection) -> Result<i64, LogError> {
        c.query_row("UPDATE media_namespaces SET revision=revision+1 WHERE id=?1 AND revision<9223372036854775807 RETURNING revision",[self.namespace],|r|r.get(0)).optional()?.ok_or(LogError::RepositoryCounterExhausted)
    }
    /// Reserve a locator before publication. A failed write leaves retryable
    /// metadata and preserves the prior plaintext eligibility, if any.
    pub(super) fn register(
        &self,
        name: &str,
        file: ConversationId,
        partial: bool,
        legacy: Option<bool>,
    ) -> std::io::Result<()> {
        validate_locator(name, file, partial).map_err(io)?;
        self.profile.database().transaction(|c|{
   let old:Option<bool>=c.query_row("SELECT legacy FROM media_files WHERE namespace=?1 AND name=?2",params![self.namespace,name],|r|r.get(0)).optional()?;
   if old.is_none(){
    let limit=i64::try_from(self.limit.unwrap_or(i64::MAX as usize)).map_err(|_|LogError::InvalidPageBudget)?;
    if c.execute("UPDATE media_namespaces SET entries=entries+1 WHERE id=?1 AND entries<?2",params![self.namespace,limit])?!=1{return Err(LogError::AdmissionRejected("media directory entry limit".into()));}
   }
   let revision=self.increment_on(c)?;
   c.execute("INSERT INTO media_files VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(namespace,name) DO UPDATE SET file=excluded.file,legacy=excluded.legacy,partial=excluded.partial,revision=excluded.revision",params![self.namespace,name,file.as_bytes().as_slice(),legacy.unwrap_or(old.unwrap_or(false)),partial,revision])?;
   Ok(())
  }).map_err(io)
    }
    pub(super) fn complete(&self, name: &str) -> std::io::Result<()> {
        self.profile
            .database()
            .transaction(|c| {
                c.execute(
                    "UPDATE media_files SET legacy=0 WHERE namespace=?1 AND name=?2",
                    params![self.namespace, name],
                )?;
                Ok(())
            })
            .map_err(io)
    }
    pub(super) fn remove(&self, name: &str, expected: Option<i64>) -> std::io::Result<()> {
        self.profile.database().transaction(|c|{
   let count=c.execute("DELETE FROM media_files WHERE namespace=?1 AND name=?2 AND (?3 IS NULL OR revision=?3)",params![self.namespace,name,expected])?;
   if count!=0{self.increment_on(c)?;c.execute("UPDATE media_namespaces SET entries=entries-?2 WHERE id=?1",params![self.namespace,count as i64])?;}
   Ok(())
  }).map_err(io)
    }
    pub(super) fn exact(
        &self,
        file: ConversationId,
        after: Option<&str>,
    ) -> std::io::Result<Vec<MediaEntry>> {
        self.profile.database().read(|c|{
   let mut query=c.prepare("SELECT name,file,legacy,partial,revision FROM media_files WHERE namespace=?1 AND file=?2 AND name>coalesce(?3,'') ORDER BY name LIMIT 32")?;
   let mut rows=query.query(params![self.namespace,file.as_bytes().as_slice(),after])?;let mut page=Vec::new();while let Some(row)=rows.next()?{page.push(entry(row)?);}Ok(page)
  }).map_err(io)
    }
    pub(super) fn page(
        &self,
        after: Option<&str>,
        revision: Option<i64>,
    ) -> std::io::Result<Vec<MediaEntry>> {
        self.profile.database().read(|c|{
   if let Some(revision)=revision {let current:i64=c.query_row("SELECT revision FROM media_namespaces WHERE id=?1",[self.namespace],|r|r.get(0))?;if current!=revision{return Err(LogError::StaleCursor);}}
   let prior:Option<Vec<u8>>=if let Some(after)=after {
       Some(c.query_row("SELECT file FROM media_files WHERE namespace=?1 AND name=?2",params![self.namespace,after],|row|row.get(0)).optional()?.ok_or(LogError::StaleCursor)?)
   } else {None};
   let seek=if prior.is_some(){"AND (file,name)>(?2,?3)"}else{"AND ?2 IS NULL AND ?3 IS NULL"};
   let mut query=c.prepare(&format!("SELECT name,file,legacy,partial,revision FROM media_files INDEXED BY media_file_ids WHERE namespace=?1 {seek} ORDER BY file,name LIMIT 32"))?;
   let mut rows=query.query(params![self.namespace,prior,after])?;let mut page=Vec::new();while let Some(row)=rows.next()?{page.push(entry(row)?);}Ok(page)
  }).map_err(io)
    }
    /// Cleanup deletes its own cursor row, so seek by the immutable name value
    /// rather than resolving the previous row as export pagination requires.
    pub(super) fn prune_page(&self, after: Option<&str>) -> std::io::Result<Vec<MediaEntry>> {
        self.profile.database().read(|c| {
            let mut query = c.prepare("SELECT name,file,legacy,partial,revision FROM media_files WHERE namespace=?1 AND name>coalesce(?2,'') ORDER BY name LIMIT 32")?;
            let mut rows = query.query(params![self.namespace, after])?;
            let mut page = Vec::new();
            while let Some(row) = rows.next()? {
                page.push(entry(row)?);
            }
            Ok(page)
        }).map_err(io)
    }
    pub(super) fn has_legacy(&self) -> std::io::Result<bool> {
        self.profile
            .database()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM media_files WHERE namespace=?1 AND legacy=1)",
                    [self.namespace],
                    |r| r.get(0),
                )?)
            })
            .map_err(io)
    }
    pub(super) fn unchanged(&self, entry: &MediaEntry) -> std::io::Result<bool> {
        self.profile.database().read(|c|Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM media_files WHERE namespace=?1 AND name=?2 AND revision=?3)",params![self.namespace,entry.name,entry.revision],|r|r.get(0))?)).map_err(io)
    }
    /// Called once by startup, after metadata import. Later writes are excluded
    /// from this pass until another startup schedules a new cutoff.
    pub(super) fn schedule_gc(&self) -> std::io::Result<()> {
        self.profile.database().transaction(|c|{c.execute("UPDATE media_namespaces SET gc_requested=revision,gc_through=CASE WHEN gc_active=0 THEN revision ELSE gc_through END,gc_after=CASE WHEN gc_active=0 THEN NULL ELSE gc_after END,gc_active=1 WHERE id=?1",[self.namespace])?;Ok(())}).map_err(io)
    }
    pub(super) fn gc_page(&self) -> std::io::Result<Vec<MediaEntry>> {
        self.profile.database().read(|c|{
   let (active,after):(bool,Option<String>)=c.query_row("SELECT gc_active,gc_after FROM media_namespaces WHERE id=?1",[self.namespace],|r|Ok((r.get(0)?,r.get(1)?)))?;
   if !active{return Ok(Vec::new());}
   // Limit scanned metadata, including post-cutoff rows, before filtering.
   let mut query=c.prepare("SELECT name,file,legacy,partial,revision FROM media_files WHERE namespace=?1 AND name>coalesce(?2,'') ORDER BY name LIMIT 32")?;
   let mut rows=query.query(params![self.namespace,after])?;let mut page=Vec::new();
   while let Some(row)=rows.next()?{let item=entry(row)?;page.push(item);}
   Ok(page)
  }).map_err(io)
    }
    pub(super) fn gc_eligible(&self, entry: &MediaEntry) -> std::io::Result<bool> {
        self.profile.database().read(|c|Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM media_namespaces n JOIN media_files f ON f.namespace=n.id WHERE n.id=?1 AND n.gc_active=1 AND f.name=?2 AND f.revision=?3 AND f.revision<=n.gc_through)",params![self.namespace,entry.name,entry.revision],|r|r.get(0))?)).map_err(io)
    }
    pub(super) fn advance_gc(&self, after: Option<&str>) -> std::io::Result<()> {
        self.profile.database().transaction(|c|{
  if let Some(after)=after{c.execute("UPDATE media_namespaces SET gc_after=?2 WHERE id=?1 AND gc_active=1",params![self.namespace,after])?;}else{c.execute("UPDATE media_namespaces SET gc_active=(gc_requested>gc_through),gc_through=gc_requested,gc_after=NULL WHERE id=?1",[self.namespace])?;}Ok(())
 }).map_err(io)
    }
    pub(super) fn gc_pending(&self) -> std::io::Result<bool> {
        self.profile
            .database()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT gc_active FROM media_namespaces WHERE id=?1",
                    [self.namespace],
                    |r| r.get(0),
                )?)
            })
            .map_err(io)
    }
}
