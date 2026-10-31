//! Independent participant event-source publication. The delivery journal remains
//! the recovery bridge while other participant stores retain their legacy owners.
use super::{
    context::ProfileStorage,
    manifest::{self, Source, invalid},
    publication, publication_files as files,
};
use crate::{
    eventlog::{
        Event, LogError,
        repository::{EventDomainId, EventRepository, import},
    },
    storage::{database::EncryptedDatabase, record_log::EncryptedRecordLog},
};
use rusqlite::{OptionalExtension, params};
use std::{io::Write, path::Path, sync::Arc};

const DOMAIN: EventDomainId = EventDomainId(*b"MeshPartEventsV1");
const SCHEMA: &str = "
CREATE TABLE participant_event_publication(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=1), domain BLOB NOT NULL CHECK(length(domain)=16), locator BLOB NOT NULL, archive BLOB, work BLOB, raw_size INTEGER NOT NULL CHECK(raw_size>=0), raw_hash BLOB CHECK(raw_hash IS NULL OR length(raw_hash)=32), expected_stub BLOB NOT NULL CHECK(length(expected_stub)=54), phase INTEGER NOT NULL CHECK(phase BETWEEN 0 AND 4), authenticated_records INTEGER NOT NULL CHECK(authenticated_records>=0), imported_records INTEGER NOT NULL DEFAULT 0 CHECK(imported_records>=0), unique_records INTEGER NOT NULL DEFAULT 0 CHECK(unique_records>=0), staging_pending INTEGER NOT NULL DEFAULT 1 CHECK(staging_pending IN (0,1)), cleanup_pending INTEGER NOT NULL DEFAULT 0 CHECK(cleanup_pending IN (0,1)));
";
fn authenticate(path: &Path, password: &str) -> Result<i64, LogError> {
    let mut records = 0i64;
    drop(EncryptedRecordLog::<Event>::open_with_visitor(
        path,
        password,
        b"MTLOG1",
        |event| {
            let _decoded = publication::DecodedRecord::new();
            publication::observe("decode");
            if !event.is_canonical() {
                return Err(LogError::NonCanonical);
            }
            if !event.verify_integrity() {
                return Err(LogError::CorruptId);
            }
            if !event.verify_signature() {
                return Err(LogError::BadSignature);
            }
            records = records
                .checked_add(1)
                .ok_or(LogError::RepositoryCounterExhausted)?;
            Ok(())
        },
    )?);
    Ok(records)
}
/// Before first control metadata publication, authenticate the existing source
/// without repairing or opening its original inode as a legacy writer.
pub(super) fn authenticate_legacy(
    root: &Path,
    path: &Path,
    password: &str,
) -> Result<(), LogError> {
    if !files::inspect_regular(path)? {
        return Ok(());
    }
    files::validate_header(path, b"MTLOG1")?;
    let work = tempfile::Builder::new()
        .prefix(".participant-auth-")
        .tempdir_in(root)?;
    let copy = work.path().join("events");
    files::copy(path, &copy)?;
    authenticate(&copy, password)?;
    Ok(())
}
fn source(db: &EncryptedDatabase) -> Result<Option<Source>, LogError> {
    db.read(|c| {
        let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='participant_event_publication')",[],|r|r.get(0))?;
        if !exists { return Ok(None); }
        manifest::validate_ddl(c,SCHEMA)?;
        let row = c.query_row("SELECT version,domain,locator,archive,work,raw_size,raw_hash,expected_stub,phase,authenticated_records FROM participant_event_publication WHERE singleton=1",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,Vec<u8>>(1)?,r.get::<_,Vec<u8>>(2)?,r.get::<_,Option<Vec<u8>>>(3)?,r.get::<_,Option<Vec<u8>>>(4)?,r.get::<_,i64>(5)?,r.get::<_,Option<Vec<u8>>>(6)?,r.get::<_,Vec<u8>>(7)?,r.get::<_,i64>(8)?,r.get::<_,i64>(9)?))).optional()?.ok_or_else(||invalid("missing participant event publication authority"))?;
        if row.0 != 1 || row.1 != DOMAIN.0 || row.7.len()!=54 || &row.7[..6]!=b"MTSDB1" || row.7[38..]!=DOMAIN.0 {return Err(invalid("incompatible participant event publication"));}
        let uuid:Vec<u8>=c.query_row("SELECT store_uuid FROM mesh_database_metadata WHERE singleton=1",[],|r|r.get(0))?;
        if row.7[6..22] != uuid {return Err(invalid("participant event barrier belongs to another store"));}
        let recorded:Vec<u8>=c.query_row("SELECT locator FROM participant_sources WHERE domain=0",[],|r|r.get(0))?;
        if row.2 != recorded {return Err(invalid("participant event source locator changed"));}
        if !(0..=4).contains(&row.8) || row.9 < 0 || row.6.is_some() != row.3.is_some() || row.3.is_some()!=row.4.is_some() { return Err(invalid("invalid participant event source progress")); }
        let (imported, unique):(i64,i64)=c.query_row("SELECT imported_records,unique_records FROM participant_event_publication WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if imported<0 || unique<0 || unique>imported || imported>row.9 || (row.8>=3 && imported!=row.9) || (row.6.is_none() && (row.9!=0 || row.5!=0)) {return Err(invalid("invalid participant event import coverage"));}
        Ok(Some(Source {source:0,locator:manifest::decode(row.2)?,archive:row.3.map(manifest::decode).transpose()?,work:row.4.map(manifest::decode).transpose()?,raw_size:u64::try_from(row.5).map_err(|_|invalid("invalid event source size"))?,raw_hash:row.6,stub:row.7,phase:row.8,authenticated_records:row.9}))
    })
}
fn prepare(profile: &ProfileStorage, path: &Path, password: &str) -> Result<(), LogError> {
    let root = profile.root();
    let locator = path
        .strip_prefix(root)
        .map_err(|_| invalid("participant event source escapes owned root"))?
        .to_path_buf();
    if files::inspect_regular(path)? {
        files::validate_header(path, b"MTLOG1")?;
    }
    let directory = tempfile::Builder::new()
        .prefix(".participant-events-")
        .tempdir_in(root)?
        .keep();
    let relative = directory
        .strip_prefix(root)
        .map_err(|_| invalid("invalid participant migration root"))?;
    let mut stub = b"MTSDB1".to_vec();
    let store_uuid = profile.database().read(|c| {
        Ok(c.query_row(
            "SELECT store_uuid FROM mesh_database_metadata WHERE singleton=1",
            [],
            |r| r.get::<_, Vec<u8>>(0),
        )?)
    })?;
    stub.extend_from_slice(&store_uuid);
    stub.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    stub.extend_from_slice(&DOMAIN.0);
    let mut source = Source {
        source: 0,
        locator,
        archive: None,
        work: None,
        raw_size: 0,
        raw_hash: None,
        stub,
        phase: 0,
        authenticated_records: 0,
    };
    if files::inspect_regular(path)? {
        source.archive = Some(relative.join("source-0.raw"));
        source.work = Some(relative.join("source-0.work"));
        let (size, hash) = files::copy(path, &root.join(source.archive.as_ref().unwrap()))?;
        source.raw_size = size;
        source.raw_hash = Some(hash);
        let work = publication::new_work(root, &source)?;
        source.authenticated_records = authenticate(&work, password)?;
        if !files::matches_raw(path, &source)? {
            return Err(invalid(
                "participant event source changed during authentication",
            ));
        }
    }
    files::sync_parent(&directory.join("source-0.raw"))?;
    profile.database().transaction(|tx| {
        tx.execute_batch(SCHEMA)?; import::initialize_on(tx)?; EventRepository::initialize_unpublished_on(tx,DOMAIN)?;
        tx.execute("INSERT INTO participant_event_publication(singleton,version,domain,locator,archive,work,raw_size,raw_hash,expected_stub,phase,authenticated_records) VALUES(1,1,?1,?2,?3,?4,?5,?6,?7,0,?8)",params![DOMAIN.0.as_slice(),manifest::encode(&source.locator),source.archive.as_deref().map(manifest::encode),source.work.as_deref().map(manifest::encode),i64::try_from(source.raw_size).map_err(|_|invalid("event source exceeds SQL range"))?,source.raw_hash,source.stub,source.authenticated_records])?; Ok(())
    })?;
    publication::cut("participant-events-prepared")
}
fn phase(db: &EncryptedDatabase, phase: i64) -> Result<(), LogError> {
    db.transaction(|tx| {
        tx.execute(
            "UPDATE participant_event_publication SET phase=?1 WHERE singleton=1",
            [phase],
        )?;
        Ok(())
    })
}
fn freeze(profile: &ProfileStorage, source: &Source, password: &str) -> Result<(), LogError> {
    let original = profile.root().join(&source.locator);
    if files::matches_stub(&original, &source.stub)? {
        return phase(profile.database(), 1);
    }
    if source.raw_hash.is_some() {
        let work = publication::new_work(profile.root(), source)?;
        if authenticate(&work, password)? != source.authenticated_records
            || !files::matches_raw(&original, source)?
        {
            return Err(invalid("participant event source changed before freeze"));
        }
    } else if files::inspect_regular(&original)? {
        return Err(invalid(
            "fresh participant event source appeared before freeze",
        ));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(profile.root())?;
    temporary.write_all(&source.stub)?;
    temporary.as_file().sync_all()?;
    let result = temporary.persist(&original).map(drop).map_err(|e| e.error);
    if !files::matches_stub(&original, &source.stub)? {
        result?;
        return Err(invalid("participant event barrier publication is unknown"));
    }
    files::sync_parent(&original)?;
    publication::cut("participant-events-barrier")?;
    phase(profile.database(), 1)
}
fn cleanup(profile: &ProfileStorage, source: &Source) -> Result<(), LogError> {
    let (staging,archives)=profile.database().read(|c|Ok(c.query_row("SELECT staging_pending,cleanup_pending FROM participant_event_publication WHERE singleton=1",[],|r|Ok((r.get::<_,bool>(0)?,r.get::<_,bool>(1)?)))?))?;
    if staging && import::reclaim_step(profile.database())? {
        profile.database().transaction(|tx| {
            tx.execute(
                "UPDATE participant_event_publication SET staging_pending=0 WHERE singleton=1",
                [],
            )?;
            Ok(())
        })?;
    }
    if !archives {
        return Ok(());
    }
    // Retain exact archives until the authoritative destination has crossed its
    // durability boundary. Filesystem cleanup can retry on the next owner open.
    if let Err(error) = profile.database().checkpoint() {
        if !profile.database().requires_reopen() && crate::storage::database::database_busy(&error)
        {
            return Ok(());
        }
        return Err(error);
    }
    for (locator, extension) in [(&source.work, "work"), (&source.archive, "raw")] {
        if let Some(locator) = locator {
            let path = files::artifact_path(profile.root(), source, locator, extension)?;
            if files::inspect_regular(&path)? {
                match std::fs::remove_file(&path) {
                    Ok(()) => files::sync_parent(&path)?,
                    Err(error) => {
                        log::warn!("participant event archive cleanup deferred: {error}");
                        return Ok(());
                    }
                }
            }
        }
    }
    publication::cut("participant-events-cleanup")?;
    profile.database().transaction(|tx| {
        tx.execute(
            "UPDATE participant_event_publication SET cleanup_pending=0 WHERE singleton=1",
            [],
        )?;
        Ok(())
    })
}
pub(super) fn maintenance(profile: &ProfileStorage) -> Result<(), LogError> {
    let source = source(profile.database())?
        .ok_or_else(|| invalid("missing participant event authority"))?;
    if source.phase != 4 {
        return Err(invalid("participant event publication is incomplete"));
    }
    cleanup(profile, &source)
}
pub(super) fn open(
    profile: &Arc<ProfileStorage>,
    path: &Path,
    password: &str,
) -> Result<EventRepository, LogError> {
    if source(profile.database())?.is_none() {
        prepare(profile, path, password)?;
    }
    let mut current = source(profile.database())?
        .ok_or_else(|| invalid("missing participant event authority"))?;
    import::validate(profile.database())?;
    EventRepository::validate_existing_domain(profile.database(), DOMAIN)?;
    if current.phase == 0 {
        freeze(profile, &current, password)?;
        current = source(profile.database())?
            .ok_or_else(|| invalid("missing participant event authority"))?;
    }
    if !files::matches_stub(path, &current.stub)? {
        return Err(invalid("participant event barrier missing or changed"));
    }
    if current.phase < 3 {
        phase(profile.database(), 2)?;
        let count = if current.raw_hash.is_some() {
            let work = publication::new_work(profile.root(), &current)?;
            import::stage(profile.database(), &work, password)?
        } else {
            import::clear(profile.database())?;
            0
        };
        if count != current.authenticated_records {
            return Err(invalid("participant event import coverage changed"));
        }
        let unique = import::rank_count(profile.database())?;
        profile.database().transaction(|tx| {tx.execute("UPDATE participant_event_publication SET phase=3,imported_records=?1,unique_records=?2 WHERE singleton=1",params![count,unique])?;Ok(())})?;
        publication::cut("participant-events-verified")?;
    }
    let repository = EventRepository::from_profile(profile.clone(), DOMAIN)?;
    let publishing = current.phase < 4;
    if publishing {
        profile.database().transaction(|tx| {
            let unique=tx.query_row("SELECT unique_records FROM participant_event_publication WHERE singleton=1 AND phase=3",[],|r|r.get(0))?;
            import::publish_count(&repository,tx,unique)?;
            super::participant::event_ownership::finish_import_on(tx)?;
            tx.execute("UPDATE participant_event_publication SET phase=4,cleanup_pending=CASE WHEN archive IS NULL THEN 0 ELSE 1 END WHERE singleton=1",[])?;
            publication::cut("participant-events-publication-transaction")?; Ok(())
        })?;
        publication::cut("participant-events-published")?;
    }
    if publishing {
        // First publication already performs an exceptional streaming import.
        // Reclaim staging in bounded transactions before returning to runtime.
        while !import::reclaim_step(profile.database())? {}
        profile.database().transaction(|tx| {
            tx.execute(
                "UPDATE participant_event_publication SET staging_pending=0 WHERE singleton=1",
                [],
            )?;
            Ok(())
        })?;
    }
    cleanup(profile, &current)?;
    Ok(repository)
}
