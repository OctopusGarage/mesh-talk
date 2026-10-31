//! Manual encrypted backup and transactional offline restore.

pub(crate) mod setup;

use crate::chat_commands::{NodeState, RuntimeAuthority};
use crate::commands::CommandError;
use crate::runtime_bootstrap::spawn_node_runtime;
use crate::state::AppState;
use mesh_talk_core::identity::{manager::IdentityManager, user::User};
use mesh_talk_core::node::Node;
use mesh_talk_core::recovery::{
    BackupError, BackupPreview, archive::ArchiveError, key::RecoveryKey,
};
use mesh_talk_core::storage::file_manager::FileManager;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

// Reuse the runtime's existing per-fixture root; production has one desktop root.
pub(crate) fn data_root(node: &NodeState) -> std::path::PathBuf {
    #[cfg(test)]
    if let Some((root, _)) = node.4.lock().unwrap().as_ref() {
        return root.clone();
    }
    #[cfg(not(test))]
    let _ = node;
    crate::data_dir()
}

const MARKER: &str = "recovery-publish.json";
const COMPLETE_MARKER: &str = "recovery-publish.complete.json";

#[derive(Serialize)]
pub struct BackupCreated {
    pub recovery_key: String,
    pub preview: BackupPreview,
    pub archive_bytes: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct BackupStatus {
    pub destination: String,
    pub preview: BackupPreview,
    pub archive_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct RestoreComplete {
    pub username: String,
    pub device_id: String,
    pub preview: BackupPreview,
}

#[derive(Serialize, Deserialize)]
struct Publication {
    username: String,
    host_id: String,
    stage: String,
    complete: bool,
}

fn backup_error(error: BackupError) -> CommandError {
    match error {
        BackupError::Archive(ArchiveError::InvalidArchive) => {
            CommandError::Crypto("Incorrect recovery key or damaged backup".into())
        }
        BackupError::Archive(ArchiveError::UnsupportedVersion) => {
            CommandError::InvalidInput("This backup version is not supported".into())
        }
        BackupError::Archive(ArchiveError::SizeLimit) => {
            CommandError::InvalidInput("Backup exceeds the supported size".into())
        }
        BackupError::Snapshot(error) => {
            CommandError::InvalidInput(format!("Backup contents are invalid: {error}"))
        }
        BackupError::Archive(ArchiveError::Io) | BackupError::Io(_) => {
            CommandError::Io("Could not read or write the backup".into())
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn sync_tree(path: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            sync_tree(&entry.path())?;
        } else if kind.is_file() {
            // FlushFileBuffers needs a writable handle on Windows. These are
            // freshly staged files owned by this process; opening does not truncate.
            std::fs::OpenOptions::new()
                .write(true)
                .open(entry.path())?
                .sync_all()?;
        } else {
            return Err(std::io::Error::other("unexpected staged file type"));
        }
    }
    sync_directory(path)
}

fn create_backup_offline(
    profile: &Path,
    password: &str,
    destination: &Path,
    scratch: &Path,
    authority: &RuntimeAuthority,
    key: &RecoveryKey,
    username: &str,
    auth_password: &str,
    record_status: bool,
    proof: &crate::app_lock::SensitiveAction,
) -> Result<BackupCreated, CommandError> {
    let parent = destination
        .parent()
        .filter(|path| path.is_dir())
        .ok_or_else(|| {
            CommandError::InvalidInput("Choose an existing destination folder".into())
        })?;
    if destination.exists() {
        return Err(CommandError::InvalidInput(
            "A backup already exists at this path".into(),
        ));
    }
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| CommandError::Io("Could not create backup file".into()))?;
    let node = Node::open_recovery_profile(profile, password).map_err(backup_error)?;
    node.write_encrypted_recovery_archive(temp.as_file_mut(), key, now_ms())
        .map_err(backup_error)?;
    temp.as_file_mut()
        .sync_all()
        .map_err(|_| CommandError::Io("Could not save backup file".into()))?;
    temp.as_file_mut()
        .seek(SeekFrom::Start(0))
        .map_err(|_| CommandError::Io("Could not verify backup file".into()))?;
    let preview = Node::inspect_encrypted_recovery_archive(temp.as_file_mut(), key, scratch)
        .map_err(backup_error)?;
    authority
        .current(|_| ())
        .map_err(CommandError::Authorization)?;
    let archive_bytes = temp
        .as_file()
        .metadata()
        .map_err(|_| CommandError::Io("Could not read backup size".into()))?
        .len();
    // `persist_noclobber` uses create-new semantics; a destination that appeared
    // during export is never overwritten.
    authority
        .current(|_| {
            proof.commit(|| {
                temp.persist_noclobber(destination).map_err(|error| {
                    if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                        CommandError::InvalidInput("A backup already exists at this path".into())
                    } else {
                        CommandError::Io("Could not publish backup file".into())
                    }
                })?;
                if sync_directory(parent).is_err() {
                    let _ = fs::remove_file(destination);
                    let _ = sync_directory(parent);
                    return Err(CommandError::Io("Could not sync backup folder".into()));
                }
                Ok(())
            })
        })
        .map_err(CommandError::Authorization)??;
    let status = BackupStatus {
        destination: destination.display().to_string(),
        preview: preview.clone(),
        archive_bytes,
    };
    // Local status contains no key. The archive itself is already durable, so a
    // failed status update does not invalidate the successful backup.
    if record_status {
        let prepared = FileManager::new(scratch.to_path_buf()).prepare_encrypted_write(
            username,
            "meta/backup_status.enc",
            &status,
            auth_password,
        );
        if let Ok(prepared) = prepared
            && let Ok(Ok(published)) = authority.current(|_| {
                proof.commit(|| {
                    prepared
                        .publish()
                        .map_err(|_| CommandError::Io("Could not record backup status".into()))
                })
            })
        {
            published.finish();
        }
    }
    Ok(BackupCreated {
        recovery_key: key.display(),
        preview,
        archive_bytes,
    })
}

/// The producer stop and offline read live in a child task so an IPC cancellation
/// cannot strand an authenticated session with its runtime stopped.
#[tauri::command]
pub async fn create_backup<R: tauri::Runtime>(
    app_handle: tauri::AppHandle<R>,
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, NodeState>,
    destination: String,
    recovery_key: Option<String>,
    password: Option<String>,
) -> Result<BackupCreated, CommandError> {
    let generation = app_state
        .session()
        .capture()
        .map_err(CommandError::Authentication)?
        .generation();
    let recovery_key = match recovery_key {
        Some(key) => key,
        None => setup::saved_key(&app_state, &node_state, generation).await?,
    };
    create_backup_with_status(
        app_handle,
        &app_state,
        &node_state,
        destination,
        recovery_key,
        true,
        Some(generation),
        password,
    )
    .await
}

pub(crate) async fn create_backup_with_status<R: tauri::Runtime>(
    app_handle: tauri::AppHandle<R>,
    app_state: &AppState,
    node_state: &NodeState,
    destination: String,
    recovery_key: String,
    record_status: bool,
    expected_generation: Option<u64>,
    verification_password: Option<String>,
) -> Result<BackupCreated, CommandError> {
    let key = RecoveryKey::parse(&recovery_key)
        .map_err(|_| CommandError::InvalidInput("Recovery key format is invalid".into()))?;
    let lease = app_state
        .session()
        .capture()
        .map_err(CommandError::Authentication)?;
    if expected_generation.is_some_and(|generation| lease.generation() != generation) {
        return Err(CommandError::Authorization(
            "Backup session was replaced".into(),
        ));
    }
    let proof = crate::app_lock::SensitiveAction::verify(
        app_state,
        &lease,
        verification_password,
        "export a Mesh-Talk backup",
    )
    .await?;
    let password = app_state
        .session()
        .matching(&lease, |info| info.node_storage_password.clone())
        .map_err(CommandError::Authentication)?;
    let username = app_state
        .session()
        .matching(&lease, |info| info.user.name.clone())
        .map_err(CommandError::Authentication)?;
    let auth_password = app_state
        .session()
        .matching(&lease, |info| info.password.clone())
        .map_err(CommandError::Authentication)?;
    let node = node_state.clone();
    let session = app_state.session().clone();
    let authority = RuntimeAuthority::request(session.clone(), lease.clone(), node.clone())
        .map_err(CommandError::Authorization)?;
    let child = tokio::spawn(async move {
        let permit = authority
            .begin()
            .await
            .map_err(CommandError::Authorization)?;
        let root = data_root(&node);
        let profile = root.join("accounts").join(lease.owner());
        let authority_for_work = authority.clone();
        let output = tokio::task::spawn_blocking(move || {
            create_backup_offline(
                &profile,
                &password,
                Path::new(&destination),
                &root,
                &authority_for_work,
                &key,
                &username,
                &auth_password,
                record_status,
                &proof,
            )
        })
        .await
        .map_err(|_| CommandError::Internal("Backup worker stopped".into()));
        drop(permit);
        if authority.current(|_| ()).is_ok() {
            let restart_password = session
                .matching(&lease, |info| info.node_storage_password.clone())
                .map_err(CommandError::Authorization)?;
            spawn_node_runtime(app_handle, lease, restart_password, node, session);
        }
        output?
    });
    child
        .await
        .map_err(|_| CommandError::Internal("Backup task stopped".into()))?
}

#[tauri::command]
pub async fn backup_status(
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, NodeState>,
) -> Result<Option<BackupStatus>, CommandError> {
    setup::with_storage(&app_state, &node_state, None, |storage| {
        if !storage.exists("meta/backup_status.enc") {
            return Ok(None);
        }
        storage
            .read("meta/backup_status.enc", "Could not read backup status")
            .map(Some)
    })
    .await
}

#[tauri::command]
pub async fn prepare_backup_key(
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, NodeState>,
) -> Result<String, CommandError> {
    Ok(setup::backup_setup(app_state, node_state)
        .await?
        .recovery_key)
}

#[tauri::command]
pub async fn inspect_backup(
    path: String,
    recovery_key: String,
) -> Result<BackupPreview, CommandError> {
    let key = RecoveryKey::parse(&recovery_key)
        .map_err(|_| CommandError::InvalidInput("Recovery key format is invalid".into()))?;
    tokio::task::spawn_blocking(move || {
        fs::create_dir_all(crate::data_dir())
            .map_err(|_| CommandError::Io("Could not create recovery scratch folder".into()))?;
        let file =
            File::open(path).map_err(|_| CommandError::Io("Could not open backup".into()))?;
        Node::inspect_encrypted_recovery_archive(file, &key, &crate::data_dir())
            .map_err(backup_error)
    })
    .await
    .map_err(|_| CommandError::Internal("Backup inspection stopped".into()))?
}

fn write_marker(root: &Path, marker: &Publication) -> Result<(), CommandError> {
    let path = root.join(MARKER);
    let mut temp = tempfile::NamedTempFile::new_in(root)
        .map_err(|_| CommandError::Io("Could not begin recovery publication".into()))?;
    serde_json::to_writer(temp.as_file_mut(), marker)
        .map_err(|_| CommandError::Io("Could not record recovery publication".into()))?;
    temp.as_file_mut()
        .flush()
        .map_err(|_| CommandError::Io("Could not flush recovery marker".into()))?;
    temp.as_file_mut()
        .sync_all()
        .map_err(|_| CommandError::Io("Could not sync recovery marker".into()))?;
    temp.persist_noclobber(path)
        .map_err(|_| CommandError::Io("Could not publish recovery marker".into()))?;
    sync_directory(root).map_err(|_| CommandError::Io("Could not sync recovery directory".into()))
}

/// Roll back an interrupted, unpublished pair on startup. A completed pair is
/// retained. The marker only ever contains a validated username and generated UUID.
pub(crate) fn recover_interrupted_publication(root: &Path) -> Result<(), String> {
    if !root.exists() {
        return Ok(());
    }
    let completed = root.join(COMPLETE_MARKER);
    if completed.exists() {
        fs::remove_file(completed).map_err(|e| e.to_string())?;
    }
    let path = root.join(MARKER);
    if path.exists() {
        let marker: Publication =
            serde_json::from_reader(File::open(&path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if !User::validate_username(&marker.username)
            || !mesh_talk_core::identity::user::User::validate_user_id(&marker.host_id)
            || !marker.stage.starts_with("recovery-stage-")
            || marker.stage.contains('/')
            || marker.stage.contains('\\')
        {
            return Err("invalid recovery publication marker".into());
        }
        if !marker.complete {
            let _ = fs::remove_dir_all(root.join("users").join(&marker.username));
            let _ = fs::remove_dir_all(root.join("accounts").join(&marker.host_id));
        }
        fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    // A crash before the marker was published can leave a staged profile or
    // decrypted snapshot behind. Neither is ever selectable; delete both.
    for entry in fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("recovery-stage-")
            && entry.file_type().map_err(|e| e.to_string())?.is_dir()
        {
            fs::remove_dir_all(entry.path()).map_err(|e| e.to_string())?;
        } else if name.starts_with("recovery-plaintext-")
            && entry.file_type().map_err(|e| e.to_string())?.is_file()
        {
            fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
        }
    }
    sync_directory(root).map_err(|e| e.to_string())?;
    Ok(())
}

/// Commit a verified staged credential/profile pair through the recovery journal.
/// Caller holds profile lifecycle and login namespace authority.
pub(crate) fn publish_staged_profile(
    root: &Path,
    stage: &Path,
    username: &str,
    host_id: &str,
) -> Result<(), CommandError> {
    let stage_name = stage
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| name.starts_with("recovery-stage-"))
        .ok_or_else(|| CommandError::Validation("Invalid account staging path".into()))?;
    if stage.parent() != Some(root)
        || !User::validate_username(username)
        || !User::validate_user_id(host_id)
    {
        return Err(CommandError::Validation(
            "Invalid account publication".into(),
        ));
    }
    let staged_profile = stage.join("accounts").join(host_id);
    sync_tree(&stage.join("users").join(username))
        .map_err(|_| CommandError::Io("Could not sync staged credentials".into()))?;
    sync_tree(&staged_profile)
        .map_err(|_| CommandError::Io("Could not sync staged account".into()))?;
    let auth_target = root.join("users").join(username);
    let account_target = root.join("accounts").join(host_id);
    if auth_target.exists() || account_target.exists() {
        return Err(CommandError::InvalidInput(
            "Recovery destination already exists".into(),
        ));
    }
    fs::create_dir_all(root.join("users"))
        .map_err(|_| CommandError::Io("Could not create users folder".into()))?;
    fs::create_dir_all(root.join("accounts"))
        .map_err(|_| CommandError::Io("Could not create accounts folder".into()))?;
    let marker = Publication {
        username: username.into(),
        host_id: host_id.into(),
        stage: stage_name.into(),
        complete: false,
    };
    write_marker(root, &marker)?;
    let publication: Result<(), CommandError> = (|| {
        fs::rename(stage.join("users").join(username), &auth_target)
            .map_err(|_| CommandError::Io("Could not publish local credentials".into()))?;
        sync_directory(&root.join("users"))
            .map_err(|_| CommandError::Io("Could not sync users folder".into()))?;
        fs::rename(&staged_profile, &account_target)
            .map_err(|_| CommandError::Io("Could not publish recovered account".into()))?;
        sync_directory(&root.join("accounts"))
            .map_err(|_| CommandError::Io("Could not sync accounts folder".into()))?;
        // Renaming the marker is the publication commit point. A crash before
        // this rename rolls back both targets; a crash after it retains both.
        fs::rename(root.join(MARKER), root.join(COMPLETE_MARKER))
            .map_err(|_| CommandError::Io("Could not complete recovery transaction".into()))?;
        let _ = sync_directory(root);
        let _ = fs::remove_file(root.join(COMPLETE_MARKER));
        let _ = sync_directory(root);
        Ok(())
    })();
    if publication.is_err() {
        recover_interrupted_publication(root).map_err(CommandError::Io)?;
    }
    publication
}

fn restore_offline(
    root: &Path,
    source: &Path,
    key: &RecoveryKey,
    username: &str,
    password: &str,
) -> Result<RestoreComplete, CommandError> {
    fs::create_dir_all(root)
        .map_err(|_| CommandError::Io("Could not create app data folder".into()))?;
    recover_interrupted_publication(root).map_err(CommandError::Io)?;
    if !User::validate_username(username) || password.trim() != password || password.len() < 8 {
        return Err(CommandError::InvalidInput(
            "Choose a valid username and password of at least 8 characters".into(),
        ));
    }
    if root.join("users").join(username).exists() {
        return Err(CommandError::InvalidInput(
            "Username already exists on this device".into(),
        ));
    }
    let stage = tempfile::Builder::new()
        .prefix("recovery-stage-")
        .tempdir_in(root)
        .map_err(|_| CommandError::Io("Could not create recovery staging".into()))?;
    let auth = IdentityManager::new(FileManager::new(stage.path().to_path_buf()));
    let user = auth
        .register_user(username, password)
        .map_err(|_| CommandError::InvalidInput("Could not create local credentials".into()))?;
    let staged_profile = stage.path().join("accounts").join(&user.user_id);
    fs::create_dir_all(staged_profile.parent().unwrap())
        .map_err(|_| CommandError::Io("Could not stage recovered account".into()))?;
    let file = File::open(source).map_err(|_| CommandError::Io("Could not open backup".into()))?;
    let (installed, preview) = Node::install_encrypted_recovery_archive(
        file,
        key,
        stage.path(),
        &staged_profile,
        password,
    )
    .map_err(backup_error)?;
    if installed.account_id() != preview.account_id {
        return Err(CommandError::Crypto(
            "Recovered account identity did not match".into(),
        ));
    }
    drop(installed);
    let reopened = Node::open_recovery_profile(&staged_profile, password).map_err(backup_error)?;
    if reopened.account_id() != preview.account_id {
        return Err(CommandError::Crypto(
            "Recovered profile could not be verified".into(),
        ));
    }
    let device_id = reopened.user_id();
    drop(reopened);
    auth.authenticate_user(username, password).map_err(|_| {
        CommandError::Auth("Restored local credentials could not be verified".into())
    })?;
    // The user has supplied this key successfully. Retain it with the staged
    // credentials so the next backup does not demand another recovery setup.
    setup::retain_restored_key(stage.path(), username, password, key)?;
    sync_tree(&stage.path().join("users").join(username))
        .map_err(|_| CommandError::Io("Could not sync restored credentials".into()))?;
    sync_tree(&staged_profile)
        .map_err(|_| CommandError::Io("Could not sync restored account".into()))?;
    IdentityManager::new(FileManager::new(root.to_path_buf()))
        .publish_new_account_login(username, || {
            publish_staged_profile(root, stage.path(), username, &user.user_id)
        })
        .map_err(|_| {
            CommandError::InvalidInput("Login name already exists on this device".into())
        })??;
    Ok(RestoreComplete {
        username: username.into(),
        device_id,
        preview,
    })
}

#[tauri::command]
pub async fn restore_backup(
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, NodeState>,
    path: String,
    recovery_key: String,
    username: String,
    new_password: String,
) -> Result<RestoreComplete, CommandError> {
    restore_backup_with_generation(
        &app_state,
        &node_state,
        path,
        recovery_key,
        username,
        new_password,
        None,
    )
    .await
}

pub(crate) async fn restore_backup_with_generation(
    app_state: &AppState,
    node_state: &NodeState,
    path: String,
    recovery_key: String,
    username: String,
    new_password: String,
    expected_generation: Option<u64>,
) -> Result<RestoreComplete, CommandError> {
    let key = RecoveryKey::parse(&recovery_key)
        .map_err(|_| CommandError::InvalidInput("Recovery key format is invalid".into()))?;
    let auth_guard = app_state.auth_operation.clone().lock_owned().await;
    if let Some(generation) = expected_generation {
        app_state
            .session()
            .unchanged(generation, || ())
            .map_err(CommandError::Authorization)?;
    }
    if app_state.session().get().is_some() {
        return Err(CommandError::Authorization(
            "Sign out before restoring a backup".into(),
        ));
    }
    let lifecycle_guard = node_state.lock_profile_lifecycle().await;
    let root = data_root(node_state);
    tokio::task::spawn_blocking(move || {
        let _auth_guard = auth_guard;
        let _lifecycle_guard = lifecycle_guard;
        restore_offline(&root, Path::new(&path), &key, &username, &new_password)
    })
    .await
    .map_err(|_| CommandError::Internal("Recovery worker stopped".into()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_talk_core::discovery::roster::Roster;
    use mesh_talk_core::identity::device::DeviceIdentity;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    #[test]
    fn interrupted_publication_removes_partial_pair() {
        let dir = tempfile::tempdir().unwrap();
        let marker = Publication {
            username: "alice".into(),
            host_id: "00000000-0000-4000-8000-000000000001".into(),
            stage: "recovery-stage-fixture".into(),
            complete: false,
        };
        fs::create_dir_all(dir.path().join("users/alice")).unwrap();
        fs::create_dir_all(dir.path().join("accounts").join(&marker.host_id)).unwrap();
        fs::create_dir_all(dir.path().join(&marker.stage)).unwrap();
        write_marker(dir.path(), &marker).unwrap();
        recover_interrupted_publication(dir.path()).unwrap();
        assert!(!dir.path().join("users/alice").exists());
        assert!(!dir.path().join("accounts").join(&marker.host_id).exists());
        assert!(!dir.path().join(MARKER).exists());
    }

    #[test]
    fn committed_publication_survives_startup_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let marker = Publication {
            username: "alice".into(),
            host_id: "00000000-0000-4000-8000-000000000001".into(),
            stage: "recovery-stage-fixture".into(),
            complete: false,
        };
        fs::create_dir_all(dir.path().join("users/alice")).unwrap();
        fs::create_dir_all(dir.path().join("accounts").join(&marker.host_id)).unwrap();
        fs::create_dir_all(dir.path().join(&marker.stage)).unwrap();
        write_marker(dir.path(), &marker).unwrap();
        fs::rename(dir.path().join(MARKER), dir.path().join(COMPLETE_MARKER)).unwrap();
        recover_interrupted_publication(dir.path()).unwrap();
        assert!(dir.path().join("users/alice").exists());
        assert!(dir.path().join("accounts").join(&marker.host_id).exists());
        assert!(!dir.path().join(&marker.stage).exists());
        assert!(!dir.path().join(COMPLETE_MARKER).exists());
    }

    #[test]
    fn offline_restore_publishes_verified_auth_and_new_device_together() {
        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join("source");
        fs::create_dir(&source_dir).unwrap();
        let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
        let (channel_tx, _channel_rx) = mpsc::unbounded_channel();
        let (file_tx, _file_rx) = mpsc::unbounded_channel();
        let source = Node::open(
            DeviceIdentity::generate(),
            Arc::new(Mutex::new(Roster::default())),
            dm_tx,
            channel_tx,
            file_tx,
            &source_dir.join("messages.log"),
            &source_dir.join("sent.log"),
            "old password",
        )
        .unwrap();
        let original_account = source.account_id();
        let original_device = source.user_id();
        let key = RecoveryKey::generate();
        let archive = dir.path().join("copy.mtbackup");
        source
            .write_encrypted_recovery_archive(File::create(&archive).unwrap(), &key, 42)
            .unwrap();
        let target = dir.path().join("target");
        let complete = restore_offline(&target, &archive, &key, "alice", "new password").unwrap();
        assert_eq!(complete.preview.account_id, original_account);
        assert_ne!(complete.device_id, original_device);
        assert!(!target.join(MARKER).exists());
        let auth = IdentityManager::new(FileManager::new(target.clone()));
        let user = auth.authenticate_user("alice", "new password").unwrap();
        let recovered = Node::open_recovery_profile(
            &target.join("accounts").join(user.user_id),
            "new password",
        )
        .unwrap();
        assert_eq!(recovered.account_id(), original_account);
        assert_eq!(recovered.user_id(), complete.device_id);
        let setup: setup::BackupSetup = FileManager::new(target.clone())
            .read_encrypted_file("alice", "meta/backup_setup.enc", "new password")
            .expect("restoration retains the supplied key for later manual backups");
        assert!(setup.confirmed);
        assert_eq!(setup.recovery_key, key.display());
    }

    #[test]
    fn occupied_username_is_never_modified() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("users/alice")).unwrap();
        fs::write(dir.path().join("users/alice/sentinel"), b"original").unwrap();
        let key = RecoveryKey::generate();
        let error = restore_offline(
            dir.path(),
            &dir.path().join("missing.mtbackup"),
            &key,
            "alice",
            "new password",
        )
        .unwrap_err();
        assert!(matches!(error, CommandError::InvalidInput(_)));
        assert_eq!(
            fs::read(dir.path().join("users/alice/sentinel")).unwrap(),
            b"original"
        );
        assert!(!dir.path().join(MARKER).exists());
    }
}
