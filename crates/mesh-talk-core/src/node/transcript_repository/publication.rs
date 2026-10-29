//! Each source has an independent recoverable import and legacy-writer barrier.
//! Imported rows remain unavailable until that source's barrier is durable.
use super::*;
use crate::storage::profile::publication_files::{
    copy, inspect_regular, matches_stub, stream_identity, sync_parent,
};
use std::{io::Write, path::PathBuf};

/// One published history source. Auxiliary owners keep their own SQL tables and
/// share this exact authenticated source registry and legacy-writer barrier.
pub(crate) struct PublishedHistorySource {
    profile: Arc<ProfileStorage>,
    path: PathBuf,
    stub: Vec<u8>,
}
impl PublishedHistorySource {
    /// Authenticate a cold source without publishing or repairing its path.
    /// Published sources need only their authenticated SQL identity and barrier;
    /// migration retries accept only the exact encrypted source already imported.
    pub(crate) fn authenticate_legacy<R: serde::Serialize + serde::de::DeserializeOwned>(
        profile: &ProfileStorage,
        path: &Path,
        password: &str,
        kind: i64,
        magic: &[u8; 6],
    ) -> Result<(), LogError> {
        Self::authenticate_legacy_with::<R>(profile, path, password, kind, magic, |_| Ok(()))
    }
    pub(crate) fn authenticate_legacy_with<R: serde::Serialize + serde::de::DeserializeOwned>(
        profile: &ProfileStorage,
        path: &Path,
        password: &str,
        kind: i64,
        magic: &[u8; 6],
        validate: impl FnMut(R) -> Result<(), LogError>,
    ) -> Result<(), LogError> {
        domain(kind)?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let path = std::fs::canonicalize(parent)?.join(
            path.file_name()
                .ok_or_else(|| invalid("history source has no filename"))?,
        );
        let present = inspect_regular(&path)?;
        let locator = manifest::encode(path.strip_prefix(profile.root()).unwrap_or(&path));
        let state = match profile.participant_database() {
            None => None,
            Some(database) => database.read(|c| {
                let initialized: bool = c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='transcript_sources')",
                    [],
                    |r| r.get(0),
                )?;
                if initialized {
                    load(c, kind, &locator)
                } else {
                    Ok(None)
                }
            })?,
        };
        if let Some(state) = state {
            if matches_stub(&path, &state.stub)? {
                return Ok(());
            }
            if state.published {
                return Err(invalid("published history source barrier changed"));
            }
            let identity = if present {
                stream_identity(&path)?
            } else {
                (0, vec![0; 32])
            };
            if identity != (state.raw_size, state.raw_hash) {
                return Err(invalid("imported history source changed"));
            }
            return Ok(());
        }
        if !present {
            return Ok(());
        }
        let snapshot = tempfile::Builder::new()
            .prefix(".history-preflight-")
            .tempdir_in(parent)?;
        let source = snapshot.path().join("source.log");
        let before = copy(&path, &source)?;
        let log = crate::storage::record_log::EncryptedRecordLog::<R>::open_with_visitor(
            &source, password, magic, validate,
        )?;
        log.sync()?;
        if !inspect_regular(&path)? || stream_identity(&path)? != before {
            return Err(invalid("history source changed during authentication"));
        }
        Ok(())
    }
    pub(crate) fn open(
        profile: Arc<ProfileStorage>,
        path: &Path,
        password: &str,
        kind: i64,
        import: impl FnOnce(&Connection, &Path, &str) -> Result<(), LogError>,
    ) -> Result<Self, LogError> {
        initialize(&profile)?;
        let (path, stub) = publish(&profile, path, password, kind, import)?;
        Ok(Self {
            profile,
            path,
            stub,
        })
    }
    pub(crate) fn profile(&self) -> &Arc<ProfileStorage> {
        &self.profile
    }
    pub(crate) fn check_write(&self) -> Result<(), LogError> {
        if !matches_stub(&self.path, &self.stub)? {
            return Err(invalid("history source barrier changed"));
        }
        sync_parent(&self.path)
    }
}

struct Source {
    stub: Vec<u8>,
    raw_size: u64,
    raw_hash: Vec<u8>,
    published: bool,
}
fn load(c: &Connection, kind: i64, locator: &[u8]) -> Result<Option<Source>, LogError> {
    let mut statement = c.prepare(
        "SELECT locator,stub,raw_size,raw_hash,published FROM transcript_sources WHERE kind=?1",
    )?;
    let mut rows = statement.query([kind])?;
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
        || source.stub[22..38] != domain(kind)?
        || source.raw_hash.len() != 32
    {
        return Err(invalid("invalid transcript publication identity"));
    }
    Ok(Some(source))
}
fn domain(kind: i64) -> Result<[u8; 16], LogError> {
    match kind {
        SENT => Ok(*b"MeshSentHistory1"),
        RECEIVED => Ok(*b"MeshRecvHistory1"),
        FILES => Ok(*b"MeshFileHistory1"),
        SUPPRESSION => Ok(*b"MeshSuppression1"),
        RECOVERED_IDS => Ok(*b"MeshRecoveredId1"),
        FILE_PROOFS => Ok(*b"MeshFileProofs01"),
        FILE_IDENTITIES => Ok(*b"MeshFileIdent001"),
        CHANNEL_SENDERS => Ok(*b"MeshChanSenders1"),
        SENT_MANIFEST_SCOPES => Ok(*b"MeshFileScopes01"),
        RECALL_MARKS => Ok(*b"MeshRecallMarks1"),
        _ => Err(invalid("invalid transcript source kind")),
    }
}
pub(super) fn publish(
    profile: &ProfileStorage,
    path: &Path,
    password: &str,
    kind: i64,
    import: impl FnOnce(&Connection, &Path, &str) -> Result<(), LogError>,
) -> Result<(PathBuf, Vec<u8>), LogError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let path = std::fs::canonicalize(parent)?.join(
        path.file_name()
            .ok_or_else(|| invalid("transcript source has no filename"))?,
    );
    inspect_regular(&path)?;
    let locator = manifest::encode(path.strip_prefix(profile.root()).unwrap_or(&path));
    let mut state = profile.database().read(|c| load(c, kind, &locator))?;
    if state.is_none() {
        // The legacy visitor can repair a torn tail. Give it only an encrypted
        // snapshot, so neither repair nor an import failure changes the source.
        let snapshot = tempfile::Builder::new()
            .prefix(".transcript-import-")
            .tempdir_in(parent)?;
        let snapshot_path = snapshot.path().join("source.log");
        let original = if inspect_regular(&path)? {
            Some(copy(&path, &snapshot_path)?)
        } else {
            None
        };
        profile.database().transaction(|c| {
            let uuid:Vec<u8>=c.query_row("SELECT store_uuid FROM mesh_database_metadata WHERE singleton=1",[],|r|r.get(0))?;
            let mut stub=b"MTSQL2".to_vec();stub.extend(uuid);stub.extend(domain(kind)?);stub.extend(uuid::Uuid::new_v4().as_bytes());
            c.execute("INSERT INTO transcript_sources(kind,locator,stub,raw_size,raw_hash,published) VALUES(?1,?2,?3,0,zeroblob(32),0)",params![kind,&locator,stub])?;
            // The visitor retains one authenticated record at a time. A failure
            // rolls back every row and leaves this source unpublished.
            import(c,&snapshot_path,password)?;
            let current = if inspect_regular(&path)? { Some(stream_identity(&path)?) } else { None };
            if current != original {
                return Err(invalid("legacy transcript source changed during import"));
            }
            // No valid legacy log is zero bytes: (0, zero hash) records exact
            // absence, without creating a legacy writer at the published path.
            let (length, hash) = original.clone().unwrap_or((0, vec![0; 32]));
            c.execute("UPDATE transcript_sources SET raw_size=?2,raw_hash=?3 WHERE kind=?1",params![kind,i64::try_from(length).map_err(|_|invalid("legacy transcript source is too large"))?,hash])?;
            Ok(())
        })?;
        cut("transcript-imported")?;
        state = profile.database().read(|c| load(c, kind, &locator))?;
    }
    let state = state.ok_or_else(|| invalid("missing transcript import authority"))?;
    if state.published {
        if !matches_stub(&path, &state.stub)? {
            return Err(invalid(
                "published transcript source barrier is missing or changed",
            ));
        }
        return Ok((path, state.stub));
    }
    if !matches_stub(&path, &state.stub)? {
        let unchanged = if state.raw_size == 0 {
            state.raw_hash == [0; 32] && !inspect_regular(&path)?
        } else {
            let (length, hash) = stream_identity(&path)?;
            length == state.raw_size && hash == state.raw_hash
        };
        if !unchanged {
            return Err(invalid(
                "legacy transcript source changed before publication",
            ));
        }
        let mut temporary = tempfile::NamedTempFile::new_in(
            path.parent()
                .ok_or_else(|| invalid("missing transcript parent"))?,
        )?;
        temporary.write_all(&state.stub)?;
        temporary.as_file().sync_all()?;
        let replacement = temporary
            .persist(&path)
            .map(drop)
            .map_err(|error| error.error);
        if !matches_stub(&path, &state.stub)? {
            return Err(replacement
                .err()
                .map(LogError::Io)
                .unwrap_or_else(|| invalid("transcript barrier replacement is uncertain")));
        }
    }
    sync_parent(&path)?;
    cut("transcript-barrier")?;
    profile.database().transaction(|c| {
        c.execute(
            "UPDATE transcript_sources SET published=1 WHERE kind=?1 AND published=0",
            [kind],
        )?;
        Ok(())
    })?;
    cut("transcript-published")?;
    Ok((path, state.stub))
}
#[cfg(test)]
thread_local! {static FAIL_AT:std::cell::Cell<Option<&'static str>>=const{std::cell::Cell::new(None)};}
#[cfg(test)]
pub(super) fn fail_at(point: Option<&'static str>) {
    FAIL_AT.with(|value| value.set(point));
}
fn cut(point: &str) -> Result<(), LogError> {
    #[cfg(test)]
    if FAIL_AT.with(|value| value.get() == Some(point)) {
        return Err(std::io::Error::other(format!("injected {point}")).into());
    }
    let _ = point;
    Ok(())
}
