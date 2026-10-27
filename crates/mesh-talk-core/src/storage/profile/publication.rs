//! Fail-closed, exceptional legacy publication. Old writers must be quiescent:
//! an advisory lease cannot fence old binaries or already-open retired inodes.
pub(super) use super::publication_files::source_layout;
use super::publication_files::{
    artifact_path, copy, inspect_regular, matches_raw, matches_stub, publish_absent, sync_parent,
    validate_header,
};
use super::{
    ProfileStorageLease,
    context::{
        ArchiveCleanupOutcome, ProfileCleanupProgress, ProfileStorage, RELAY_ARRIVALS, RELAY_EVENTS,
    },
    manifest::{self, Source, invalid},
};
use crate::{
    eventlog::{
        Event, LogError,
        repository::{EventRepository, import},
    },
    postoffice::storage as arrivals,
    storage::{database::EncryptedDatabase, record_log::EncryptedRecordLog},
};
use rusqlite::params;
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

fn new_work(root: &Path, source: &Source) -> Result<PathBuf, LogError> {
    let archive = artifact_path(
        root,
        source,
        source
            .archive
            .as_ref()
            .ok_or_else(|| invalid("missing raw archive locator"))?,
        "raw",
    )?;
    if !matches_raw(&archive, source)? {
        return Err(invalid("raw archive identity mismatch"));
    }
    let work = artifact_path(
        root,
        source,
        source
            .work
            .as_ref()
            .ok_or_else(|| invalid("missing disposable work locator"))?,
        "work",
    )?;
    // Only these authenticated exact manifest-owned disposable paths are removed.
    if inspect_regular(&work)? {
        fs::remove_file(&work)?;
    }
    if copy(&archive, &work)?
        != (
            source.raw_size,
            source
                .raw_hash
                .clone()
                .ok_or_else(|| invalid("missing raw archive hash"))?,
        )
    {
        return Err(invalid("work copy identity mismatch"));
    }
    sync_parent(&work)?;
    Ok(work)
}
fn authenticate(root: &Path, source: &Source, password: &str) -> Result<i64, LogError> {
    let work = new_work(root, source)?;
    let mut count = 0i64;
    if source.source == 0 {
        drop(EncryptedRecordLog::<Event>::open_with_visitor(
            &work,
            password,
            b"MTLOG1",
            |_| {
                let _decoded = DecodedRecord::new();
                observe("decode");
                count = count
                    .checked_add(1)
                    .ok_or(LogError::RepositoryCounterExhausted)?;
                Ok(())
            },
        )?);
    } else {
        arrivals::authenticate(&work, password, || {
            observe("decode");
            count = count
                .checked_add(1)
                .ok_or(LogError::RepositoryCounterExhausted)?;
            Ok(())
        })?;
    }
    Ok(count)
}
fn bootstrap(
    lease: &ProfileStorageLease,
    paths: &[PathBuf; 2],
    password: &str,
    binding: &[u8],
) -> Result<EncryptedDatabase, LogError> {
    let root = lease.canonical_root();
    for (index, magic) in [b"MTLOG1", b"MTPOR1"].iter().enumerate() {
        let path = root.join(&paths[index]);
        if inspect_regular(&path)? {
            validate_header(&path, magic)?;
        }
    }
    let directory = tempfile::Builder::new()
        .prefix(".relay-publication-")
        .tempdir_in(root)?;
    // Surviving candidates are never discovered/deleted by guessing names. Their
    // source paths stay untouched until an authenticated final manifest exists.
    let directory = directory.keep();
    let relative = directory
        .strip_prefix(root)
        .map_err(|_| invalid("invalid private candidate root"))?;
    let store_uuid = uuid::Uuid::new_v4();
    let mut sources = Vec::with_capacity(2);
    for (index, domain) in [RELAY_EVENTS.0, RELAY_ARRIVALS].iter().enumerate() {
        let source_uuid = uuid::Uuid::new_v4();
        let mut stub = b"MTSDB1".to_vec();
        stub.extend_from_slice(store_uuid.as_bytes());
        stub.extend_from_slice(source_uuid.as_bytes());
        stub.extend_from_slice(domain);
        let mut source = Source {
            source: index as i64,
            locator: paths[index].clone(),
            archive: None,
            work: None,
            raw_size: 0,
            raw_hash: None,
            stub,
            phase: manifest::PREPARED,
            authenticated_records: 0,
        };
        let original = root.join(&source.locator);
        if inspect_regular(&original)? {
            let archive = relative.join(format!("source-{index}.raw"));
            let (size, hash) = copy(&original, &root.join(&archive))?;
            source.raw_size = size;
            source.raw_hash = Some(hash);
            source.archive = Some(archive);
            source.work = Some(relative.join(format!("source-{index}.work")));
            source.authenticated_records = authenticate(root, &source, password)?;
            if !matches_raw(&original, &source)? {
                return Err(invalid("legacy source changed during authentication"));
            }
        }
        sources.push((source, source_uuid));
    }
    sync_parent(&directory.join("source-0.raw"))?;
    cut("archive")?;
    let candidate = directory.join("candidate.db");
    let db = EncryptedDatabase::open(&candidate, password, binding)?;
    manifest::create(&db)?;
    db.transaction(|tx| {
        // Use the database's authenticated UUID in the barrier, never a filename
        // or a current account. The pre-created UUID is source-local randomness.
        let actual:Vec<u8>=tx.query_row("SELECT store_uuid FROM mesh_database_metadata WHERE singleton=1",[],|r|r.get(0))?;
        for (source,uuid) in &mut sources {
            source.stub[6..22].copy_from_slice(&actual);
            tx.execute("INSERT INTO relay_sources(source,domain,source_uuid,locator,archive,work,raw_size,raw_hash,expected_stub,phase,authenticated_records) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,0,?10)",params![source.source,[RELAY_EVENTS.0,RELAY_ARRIVALS][source.source as usize].as_slice(),uuid.as_bytes().as_slice(),manifest::encode(&source.locator),source.archive.as_deref().map(manifest::encode),source.work.as_deref().map(manifest::encode),i64::try_from(source.raw_size).map_err(|_|invalid("source size exceeds SQL range"))?,source.raw_hash,source.stub,source.authenticated_records])?;
        } Ok(())
    })?;
    import::initialize(&db)?;
    arrivals::initialize(&db)?;
    EventRepository::initialize_unpublished_domain(&db, RELAY_EVENTS)?;
    db.checkpoint_and_close()?;
    cut("closed-candidate")?;
    // Windows FlushFileBuffers requires a handle opened with write access.
    File::options()
        .read(true)
        .write(true)
        .open(&candidate)?
        .sync_all()?;
    // Reopen and authenticate the closed self-contained candidate before moving.
    EncryptedDatabase::open(&candidate, password, binding)?.checkpoint_and_close()?;
    publish_absent(&candidate, &root.join("profile.db"))?;
    sync_parent(&root.join("profile.db"))?;
    cut("prepared")?;
    EncryptedDatabase::open(&root.join("profile.db"), password, binding)
}
pub(super) fn open_database(
    lease: &ProfileStorageLease,
    paths: &[PathBuf; 2],
    password: &str,
    binding: &[u8],
) -> Result<EncryptedDatabase, LogError> {
    if inspect_regular(&lease.canonical_root().join("profile.db"))? {
        EncryptedDatabase::open(
            &lease.canonical_root().join("profile.db"),
            password,
            binding,
        )
    } else {
        bootstrap(lease, paths, password, binding)
    }
}

fn freeze(profile: &ProfileStorage, source: &Source, password: &str) -> Result<(), LogError> {
    let original = profile.root().join(&source.locator);
    if matches_stub(&original, &source.stub)? {
        return manifest::phase(profile.database(), source.source, manifest::FROZEN);
    }
    if source.raw_hash.is_some() {
        if authenticate(profile.root(), source, password)? != source.authenticated_records {
            return Err(invalid("legacy credential record count changed"));
        }
        if !matches_raw(&original, source)? {
            return Err(invalid("legacy source identity changed before freeze"));
        }
    } else if inspect_regular(&original)? {
        return Err(invalid("fresh source appeared before freeze"));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(profile.root())?;
    temporary.write_all(&source.stub)?;
    temporary.as_file().sync_all()?;
    let result = temporary.persist(&original).map(drop).map_err(|e| e.error);
    // A failed replacement may nevertheless have installed the target. Inspect
    // actual bytes, never infer an empty source or fall back to legacy runtime.
    if !matches_stub(&original, &source.stub)? {
        if let Err(error) = result {
            if source.raw_hash.is_some() && matches_raw(&original, source)? {
                return Err(error.into());
            }
            if source.raw_hash.is_none() && !inspect_regular(&original)? {
                return Err(error.into());
            }
            return Err(invalid(
                "source replacement failed with an unknown path identity",
            ));
        }
        return Err(invalid("source barrier publication outcome is unknown"));
    }
    sync_parent(&original)?;
    cut(if source.source == 0 {
        "event-stub"
    } else {
        "arrival-stub"
    })?;
    manifest::phase(profile.database(), source.source, manifest::FROZEN)?;
    cut("frozen")
}
fn validate_barriers(profile: &ProfileStorage, sources: &[Source]) -> Result<(), LogError> {
    for source in sources {
        if !matches_stub(&profile.root().join(&source.locator), &source.stub)? {
            return Err(invalid("published source barrier is missing or altered"));
        }
    }
    Ok(())
}
pub(super) fn complete(
    profile: &Arc<ProfileStorage>,
    password: &str,
    now: u64,
) -> Result<(), LogError> {
    let sources = manifest::sources(profile.database())?;
    import::validate(profile.database())?;
    arrivals::validate(profile.database())?;
    if sources.iter().all(|s| s.phase == manifest::PUBLISHED) {
        profile
            .database()
            .read(|c| arrivals::require_ready(c).map(|_| ()))?;
        validate_barriers(profile, &sources)?;
        cleanup(profile, &sources)?;
        return Ok(());
    }
    if sources.iter().any(|s| s.phase == manifest::PUBLISHED) {
        return Err(invalid("partially published relay sources"));
    }
    for source in &sources {
        if source.phase == manifest::PREPARED {
            freeze(profile, source, password)?;
        } else if !matches_stub(&profile.root().join(&source.locator), &source.stub)? {
            return Err(invalid("frozen source barrier is missing or altered"));
        }
    }
    let sources = manifest::sources(profile.database())?;
    for source in &sources {
        if source.phase >= manifest::VERIFIED {
            continue;
        }
        manifest::phase(profile.database(), source.source, manifest::IMPORTING)?;
        let records = if source.raw_hash.is_some() {
            let work = new_work(profile.root(), source)?;
            if source.source == 0 {
                import::stage(profile.database(), &work, password)?
            } else {
                arrivals::stage(profile.database(), &work, password)?
            }
        } else {
            if source.source == 0 {
                import::clear(profile.database())?;
            } else {
                arrivals::clear(profile.database())?;
            }
            0
        };
        if records != source.authenticated_records {
            return Err(invalid("import coverage differs from authenticated source"));
        }
        if source.source == 0 {
            cut("events-staged")?;
            import::rank(profile.database())?;
            cut("events-ranked")?;
        }
        profile.database().transaction(|tx| {
            tx.execute(
                "UPDATE relay_sources SET phase=3, imported_records=?1 WHERE source=?2",
                params![records, source.source],
            )?;
            Ok(())
        })?;
        cut("verified")?;
    }
    let sources = manifest::sources(profile.database())?;
    validate_barriers(profile, &sources)?;
    let repository = EventRepository::from_profile(profile.clone(), RELAY_EVENTS)?;
    profile.database().transaction(|tx| {
        import::publish(&repository,tx)?;
        arrivals::publish(tx,&repository,now)?;
        tx.execute("UPDATE relay_sources SET phase=4,cleanup_pending=CASE WHEN archive IS NULL THEN 0 ELSE 1 END WHERE phase=3",[])?;
        let count:i64=tx.query_row("SELECT count(*) FROM relay_sources WHERE phase=4",[],|r|r.get(0))?;
        if count!=2 { return Err(invalid("mandatory source publication incomplete")); }
        cut("publication-transaction")?; Ok(())
    })?;
    drop(repository);
    cut("published")?;
    cleanup(profile, &sources)?;
    // Exceptional migration finishes reclaiming its staging with bounded
    // transactions. Published ordinary reopen performs only one bounded step.
    while !reclaim_step(profile.database())? {}
    cut("reclaimed")?;
    Ok(())
}
fn reclaim_step(db: &EncryptedDatabase) -> Result<bool, LogError> {
    let pending: bool = db.read(|c| {
        Ok(c.query_row(
            "SELECT staging_pending FROM relay_publication WHERE singleton=1",
            [],
            |r| r.get(0),
        )?)
    })?;
    if !pending {
        return Ok(true);
    }
    let events = import::reclaim_step(db)?;
    let arrivals = arrivals::reclaim_step(db)?;
    if events && arrivals {
        db.transaction(|tx| {
            tx.execute(
                "UPDATE relay_publication SET staging_pending=0 WHERE singleton=1",
                [],
            )?;
            Ok(())
        })?;
    }
    Ok(events && arrivals)
}
/// Normal runtime continuation after a crash following publication. Never
/// drains staging to completion or skips the authenticated publication barriers.
pub(super) fn resume_cleanup_step(
    profile: &ProfileStorage,
    archive_due: bool,
) -> Result<ProfileCleanupProgress, LogError> {
    manifest::require_published(profile.database())?;
    let sources = manifest::sources(profile.database())?;
    validate_barriers(profile, &sources)?;
    let staging_pending = !reclaim_step(profile.database())?;
    let archive = if !profile.archive_cleanup_pending()? {
        ArchiveCleanupOutcome::Complete
    } else if archive_due {
        cleanup_archives(profile, &sources)?
    } else {
        ArchiveCleanupOutcome::Waiting
    };
    Ok(ProfileCleanupProgress {
        staging_pending,
        archive,
    })
}
fn cleanup(profile: &ProfileStorage, sources: &[Source]) -> Result<(), LogError> {
    reclaim_step(profile.database())?;
    // Initial publication/reopen is best effort only for an explicitly deferred
    // archive operation. DB and provenance errors still fail the constructor.
    cleanup_archives(profile, sources).map(|_| ())
}
fn defer_archive(
    profile: &ProfileStorage,
    cause: LogError,
) -> Result<ArchiveCleanupOutcome, LogError> {
    if profile.database().requires_reopen() {
        return Err(cause);
    }
    profile.database().transaction(|tx| {
        tx.execute("UPDATE relay_sources SET cleanup_error='archive cleanup remains pending' WHERE cleanup_pending=1 AND cleanup_error IS NOT 'archive cleanup remains pending'", [])?;
        Ok(())
    })?;
    Ok(ArchiveCleanupOutcome::Deferred(cause))
}
fn cleanup_archives(
    profile: &ProfileStorage,
    sources: &[Source],
) -> Result<ArchiveCleanupOutcome, LogError> {
    if !profile.archive_cleanup_pending()? {
        return Ok(ArchiveCleanupOutcome::Complete);
    }
    #[cfg(test)]
    tests::ARCHIVE_ATTEMPTS.with(|v| v.borrow_mut().push(tokio::time::Instant::now()));
    // No artifact is removed until this durability boundary succeeds. Only the
    // checkpoint's typed BUSY/LOCKED outcome is retryable; other DB errors retain
    // their original source, including faulted/poisoned owner state.
    if let Err(error) = profile.database().checkpoint() {
        return if !profile.database().requires_reopen()
            && crate::storage::database::database_busy(&error)
        {
            defer_archive(profile, error)
        } else {
            Err(error)
        };
    }
    if let Err(error) = cut("checkpoint") {
        return defer_archive(profile, error);
    }
    // Validate every fixed manifest-owned artifact before unlinking any. Actual
    // provenance rejection is authoritative; unavailable filesystem IO can defer.
    let paths = (|| {
        let mut paths = Vec::with_capacity(4);
        for source in sources {
            for (locator, extension) in [(&source.work, "work"), (&source.archive, "raw")] {
                if let Some(locator) = locator {
                    let path = artifact_path(profile.root(), source, locator, extension)?;
                    if inspect_regular(&path)? {
                        paths.push(path);
                    }
                }
            }
        }
        Ok::<_, LogError>(paths)
    })();
    let paths = match paths {
        Ok(paths) => paths,
        Err(error @ LogError::Io(_)) => return defer_archive(profile, error),
        Err(error) => return Err(error),
    };
    for path in paths {
        if let Err(error) = fs::remove_file(&path) {
            return defer_archive(profile, error.into());
        }
        if let Err(error) = sync_parent(&path) {
            return defer_archive(profile, error);
        }
    }
    if let Err(error) = cut("cleanup") {
        return defer_archive(profile, error);
    }
    // A failed final transaction is an authoritative database failure, never a
    // request to write a second diagnostic through a now-unusable connection.
    profile.database().transaction(|tx| {
        tx.execute("UPDATE relay_sources SET cleanup_pending=0,cleanup_error=NULL WHERE cleanup_pending=1 OR cleanup_error IS NOT NULL", [])?;
        Ok(())
    })?;
    Ok(ArchiveCleanupOutcome::Complete)
}

pub(crate) struct DecodedRecord;
impl DecodedRecord {
    pub(crate) fn new() -> Self {
        #[cfg(test)]
        tests::enter();
        Self
    }
}
impl Drop for DecodedRecord {
    fn drop(&mut self) {
        #[cfg(test)]
        tests::leave();
    }
}
pub(crate) fn observe_edges(count: usize) {
    #[cfg(test)]
    tests::MAX_EDGES.with(|v| v.set(v.get().max(count)));
    #[cfg(not(test))]
    let _ = count;
}
pub(crate) fn observe(kind: &str) {
    #[cfg(test)]
    tests::observe(kind);
    #[cfg(not(test))]
    let _ = kind;
}
pub(crate) fn cut(point: &str) -> Result<(), LogError> {
    #[cfg(test)]
    tests::cut(point)?;
    #[cfg(not(test))]
    let _ = point;
    Ok(())
}
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    thread_local! { pub static ARCHIVE_ATTEMPTS:RefCell<Vec<tokio::time::Instant>>=const {RefCell::new(Vec::new())}; pub static CUT: RefCell<Option<&'static str>>=const {RefCell::new(None)}; pub static HASHES:Cell<usize>=const {Cell::new(0)}; pub static DECODES:Cell<usize>=const {Cell::new(0)}; pub static LIVE:Cell<usize>=const {Cell::new(0)}; pub static MAX_LIVE:Cell<usize>=const {Cell::new(0)}; pub static MAX_BATCH:Cell<usize>=const {Cell::new(0)}; pub static MAX_EDGES:Cell<usize>=const {Cell::new(0)}; }
    pub fn enter() {
        LIVE.with(|v| {
            v.set(v.get() + 1);
            MAX_LIVE.with(|m| m.set(m.get().max(v.get())));
        });
    }
    pub fn leave() {
        LIVE.with(|v| v.set(v.get() - 1));
    }
    pub fn observe(kind: &str) {
        match kind {
            "hash" => HASHES.with(|v| v.set(v.get() + 1)),
            "decode" => DECODES.with(|v| v.set(v.get() + 1)),
            "batch" => MAX_BATCH.with(|v| v.set(v.get().max(1))),
            _ => {}
        }
    }
    thread_local! {static TIMING:RefCell<Option<std::time::Instant>>=const{RefCell::new(None)};static PHASES:RefCell<Vec<(&'static str,u128)>>=const{RefCell::new(Vec::new())};}
    pub fn start_timing() {
        TIMING.with(|v| v.replace(Some(std::time::Instant::now())));
        PHASES.with(|v| v.borrow_mut().clear());
    }
    pub fn finish_timing() -> Vec<(&'static str, u128)> {
        TIMING.with(|v| v.replace(None));
        PHASES.with(|v| std::mem::take(&mut *v.borrow_mut()))
    }
    fn phase(point: &str) {
        let name = match point {
            "archive" => "archives_authenticated",
            "prepared" => "prepared",
            "frozen" => "sources_frozen",
            "events-staged" => "events_staged",
            "events-ranked" => "events_ranked",
            "verified" => "sources_verified",
            "published" => "published",
            "cleanup" => "raw_archives_removed",
            "reclaimed" => "staging_reclaimed",
            _ => return,
        };
        TIMING.with(|start| {
            if let Some(start) = *start.borrow() {
                PHASES.with(|phases| {
                    let mut phases = phases.borrow_mut();
                    let elapsed = start.elapsed().as_millis();
                    if let Some(entry) = phases.iter_mut().find(|(label, _)| *label == name) {
                        entry.1 = elapsed;
                    } else {
                        phases.push((name, elapsed));
                    }
                });
            }
        });
    }
    pub fn cut(point: &str) -> Result<(), LogError> {
        phase(point);
        CUT.with(|v| {
            if *v.borrow() == Some(point) {
                v.replace(None);
                return Err(invalid("injected publication interruption"));
            }
            Ok(())
        })
    }
}
