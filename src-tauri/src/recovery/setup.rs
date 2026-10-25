//! Local backup setup, encrypted with the existing local-user storage password.
use super::*;

const SETUP_FILE: &str = "meta/backup_setup.enc";

#[derive(Serialize, Deserialize)]
pub struct BackupSetup {
    pub recovery_key: String,
    pub confirmed: bool,
    pub destination: String,
}

/// Serialize credential reads/writes with both exact-session and app-lock
/// authority. Blocking KDF/file I/O never runs on the async executor.
pub(crate) async fn with_storage<T: Send + 'static>(
    app: &AppState,
    node: &NodeState,
    expected_generation: Option<u64>,
    operation: impl FnOnce(&FileManager, &str, &str) -> Result<T, CommandError> + Send + 'static,
) -> Result<T, CommandError> {
    let session = app.session().clone();
    let lease = session.capture().map_err(CommandError::Authentication)?;
    if expected_generation.is_some_and(|generation| generation != lease.generation()) {
        return Err(CommandError::Authorization(
            "Backup session was replaced".into(),
        ));
    }
    let lock = app.app_lock().clone();
    let revision = lock.authorization_revision()?;
    let root = data_root(node);
    tokio::task::spawn_blocking(move || {
        session
            .matching(&lease, |info| {
                lock.with_unchanged_revision(revision, || {
                    operation(
                        &FileManager::new(root),
                        &info.user.name,
                        info.password.trim(),
                    )
                })
            })
            .map_err(CommandError::Authorization)?
    })
    .await
    .map_err(|_| CommandError::Internal("Backup setup worker stopped".into()))?
}

fn load(
    manager: &FileManager,
    username: &str,
    password: &str,
) -> Result<BackupSetup, CommandError> {
    if manager.file_exists(username, SETUP_FILE) {
        let setup: BackupSetup = manager.read_encrypted_file(username, SETUP_FILE, password)
            .map_err(|_| CommandError::Io("Could not unlock saved backup setup. Retry without replacing your recovery key.".into()))?;
        RecoveryKey::parse(&setup.recovery_key)
            .map_err(|_| CommandError::Crypto("Saved recovery key is invalid".into()))?;
        validate_destination(&setup.destination)?;
        return Ok(setup);
    }
    let setup = BackupSetup {
        recovery_key: RecoveryKey::generate().display(),
        confirmed: false,
        destination: "local".into(),
    };
    save(manager, username, password, &setup)?;
    Ok(setup)
}

fn save(
    manager: &FileManager,
    username: &str,
    password: &str,
    setup: &BackupSetup,
) -> Result<(), CommandError> {
    manager
        .write_encrypted_file(username, SETUP_FILE, setup, password)
        .map_err(|_| CommandError::Io("Could not save backup setup. Try again.".into()))
}

pub(super) fn retain_restored_key(
    root: &Path,
    username: &str,
    password: &str,
    key: &RecoveryKey,
) -> Result<(), CommandError> {
    save(
        &FileManager::new(root.to_path_buf()),
        username,
        password,
        &BackupSetup {
            recovery_key: key.display(),
            confirmed: true,
            destination: "local".into(),
        },
    )
}

fn validate_destination(destination: &str) -> Result<(), CommandError> {
    if !matches!(destination, "local" | "drive") {
        return Err(CommandError::InvalidInput(
            "Choose Local file or Google Drive".into(),
        ));
    }
    Ok(())
}

#[tauri::command]
pub async fn backup_setup(
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, NodeState>,
) -> Result<BackupSetup, CommandError> {
    with_storage(&app_state, &node_state, None, load).await
}

#[tauri::command]
pub async fn confirm_backup_key(
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, NodeState>,
    recovery_key: String,
) -> Result<(), CommandError> {
    with_storage(
        &app_state,
        &node_state,
        None,
        move |manager, username, password| {
            let mut setup = load(manager, username, password)?;
            let supplied = RecoveryKey::parse(&recovery_key)
                .map_err(|_| CommandError::InvalidInput("Recovery key format is invalid".into()))?;
            if supplied.display() != setup.recovery_key {
                return Err(CommandError::Authorization(
                    "Backup setup changed. Reopen it and save the current recovery key.".into(),
                ));
            }
            setup.confirmed = true;
            save(manager, username, password, &setup)
        },
    )
    .await
}

#[tauri::command]
pub async fn set_backup_destination(
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, NodeState>,
    destination: String,
) -> Result<(), CommandError> {
    validate_destination(&destination)?;
    with_storage(
        &app_state,
        &node_state,
        None,
        move |manager, username, password| {
            let mut setup = load(manager, username, password)?;
            setup.destination = destination;
            save(manager, username, password, &setup)
        },
    )
    .await
}

pub(crate) async fn saved_key(
    app: &AppState,
    node: &NodeState,
    generation: u64,
) -> Result<String, CommandError> {
    with_storage(
        app,
        node,
        Some(generation),
        |manager, username, password| {
            let setup = load(manager, username, password)?;
            if !setup.confirmed {
                return Err(CommandError::InvalidInput(
                    "Save your recovery key and confirm it once before backing up.".into(),
                ));
            }
            Ok(setup.recovery_key)
        },
    )
    .await
}
