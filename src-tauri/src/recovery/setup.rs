//! Local backup setup, encrypted with the existing local-user storage password.
use super::*;

const SETUP_FILE: &str = "meta/backup_setup.enc";

#[derive(Serialize, Deserialize)]
pub struct BackupSetup {
    pub recovery_key: String,
    pub confirmed: bool,
    pub destination: String,
}

/// One serialized storage operation with short publication and result authority.
/// Credentials and staged encrypted bytes never escape this blocking worker.
pub(crate) struct SessionStorage {
    manager: FileManager,
    username: String,
    password: zeroize::Zeroizing<String>,
    session: crate::state::SessionState,
    lease: crate::state::SessionLease,
    lock: crate::app_lock::AppLock,
    revision: u64,
}

impl SessionStorage {
    fn authorize<T>(
        &self,
        operation: impl FnOnce() -> Result<T, CommandError>,
    ) -> Result<T, CommandError> {
        self.session
            .matching(&self.lease, |_| {
                self.lock.with_unchanged_revision(self.revision, operation)
            })
            .map_err(CommandError::Authorization)?
    }

    pub(crate) fn exists(&self, path: &str) -> bool {
        self.manager.file_exists(&self.username, path)
    }

    pub(crate) fn read<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        message: &str,
    ) -> Result<T, CommandError> {
        self.manager
            .read_encrypted_file(&self.username, path, &self.password)
            .map_err(|_| CommandError::Io(message.into()))
    }

    pub(crate) fn write<T: Serialize>(
        &self,
        path: &str,
        value: &T,
        message: &str,
    ) -> Result<(), CommandError> {
        self.write_if(path, value, message, || Ok(()))
    }

    pub(crate) fn write_if<T: Serialize>(
        &self,
        path: &str,
        value: &T,
        message: &str,
        check: impl FnOnce() -> Result<(), CommandError>,
    ) -> Result<(), CommandError> {
        let prepared = self
            .manager
            .prepare_encrypted_write(&self.username, path, value, &self.password)
            .map_err(|_| CommandError::Io(message.into()))?;
        let published = self.authorize(|| {
            check()?;
            prepared
                .publish()
                .map_err(|_| CommandError::Io(message.into()))
        })?;
        published.finish();
        Ok(())
    }

    pub(crate) fn delete(&self, path: &str, message: &str) -> Result<(), CommandError> {
        self.authorize(|| {
            self.manager
                .delete_file(&self.username, path)
                .map_err(|_| CommandError::Io(message.into()))
        })
    }
}

/// Keep read/modify/write serialization inside the noncancellable worker. The
/// session and app lock are consulted only for capture, publication and release.
pub(crate) async fn with_storage<T: Send + 'static>(
    app: &AppState,
    node: &NodeState,
    expected_generation: Option<u64>,
    operation: impl FnOnce(&SessionStorage) -> Result<T, CommandError> + Send + 'static,
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
    let serialized = app.credential_storage.clone().lock_owned().await;
    tokio::task::spawn_blocking(move || {
        let _serialized = serialized;
        let (username, password) = session
            .matching(&lease, |info| {
                lock.with_unchanged_revision(revision, || {
                    Ok((
                        info.user.name.clone(),
                        zeroize::Zeroizing::new(info.password.trim().to_owned()),
                    ))
                })
            })
            .map_err(CommandError::Authorization)??;
        let storage = SessionStorage {
            manager: FileManager::new(root),
            username,
            password,
            session,
            lease,
            lock,
            revision,
        };
        let result = operation(&storage)?;
        storage.authorize(|| Ok(result))
    })
    .await
    .map_err(|_| CommandError::Internal("Backup setup worker stopped".into()))?
}

fn load(storage: &SessionStorage) -> Result<BackupSetup, CommandError> {
    if storage.exists(SETUP_FILE) {
        let setup: BackupSetup = storage.read(
            SETUP_FILE,
            "Could not unlock saved backup setup. Retry without replacing your recovery key.",
        )?;
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
    save(storage, &setup)?;
    Ok(setup)
}

fn save(storage: &SessionStorage, setup: &BackupSetup) -> Result<(), CommandError> {
    storage.write(SETUP_FILE, setup, "Could not save backup setup. Try again.")
}

pub(super) fn retain_restored_key(
    root: &Path,
    username: &str,
    password: &str,
    key: &RecoveryKey,
) -> Result<(), CommandError> {
    // The profile is still unpublished and has no live desktop session.
    FileManager::new(root.to_path_buf())
        .write_encrypted_file(
            username,
            SETUP_FILE,
            &BackupSetup {
                recovery_key: key.display(),
                confirmed: true,
                destination: "local".into(),
            },
            password,
        )
        .map_err(|_| CommandError::Io("Could not save backup setup. Try again.".into()))
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
    with_storage(&app_state, &node_state, None, move |storage| {
        let mut setup = load(storage)?;
        let supplied = RecoveryKey::parse(&recovery_key)
            .map_err(|_| CommandError::InvalidInput("Recovery key format is invalid".into()))?;
        if supplied.display() != setup.recovery_key {
            return Err(CommandError::Authorization(
                "Backup setup changed. Reopen it and save the current recovery key.".into(),
            ));
        }
        setup.confirmed = true;
        save(storage, &setup)
    })
    .await
}

#[tauri::command]
pub async fn set_backup_destination(
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, NodeState>,
    destination: String,
) -> Result<(), CommandError> {
    validate_destination(&destination)?;
    with_storage(&app_state, &node_state, None, move |storage| {
        let mut setup = load(storage)?;
        setup.destination = destination;
        save(storage, &setup)
    })
    .await
}

pub(crate) async fn saved_key(
    app: &AppState,
    node: &NodeState,
    generation: u64,
) -> Result<String, CommandError> {
    with_storage(app, node, Some(generation), |storage| {
        let setup = load(storage)?;
        if !setup.confirmed {
            return Err(CommandError::InvalidInput(
                "Save your recovery key and confirm it once before backing up.".into(),
            ));
        }
        Ok(setup.recovery_key)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, time::Duration};

    fn fixture() -> (tempfile::TempDir, AppState, NodeState) {
        let root = tempfile::tempdir().unwrap();
        let app = AppState::new(crate::services::auth_service::AuthService::new(Arc::new(
            IdentityManager::new(FileManager::new(root.path().to_path_buf())),
        )));
        app.session().set(
            "token".into(),
            crate::services::user::User {
                user_id: "alice".into(),
                name: "alice".into(),
                account_login_name: None,
                display_name: "Alice".into(),
                address: "fixture".into(),
                created_at: 0,
                last_seen: 0,
                is_online: true,
            },
            "pw".into(),
        );
        let node = NodeState::empty();
        *node.4.lock().unwrap() = Some((root.path().to_path_buf(), 0));
        (root, app, node)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn credential_storage_leaves_session_and_app_lock_available_during_work() {
        let (_root, app, node) = fixture();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker_app = app.clone();
        let worker = tokio::spawn(async move {
            with_storage(&worker_app, &node, None, move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            })
            .await
        });
        entered_rx.await.unwrap();
        let mut observation = tokio::task::spawn_blocking(move || {
            assert!(app.session().get().is_some());
            assert!(!app.app_lock().is_locked());
        });
        let observed = tokio::time::timeout(Duration::from_millis(150), &mut observation).await;
        let responsive = observed.is_ok();
        release_tx.send(()).unwrap();
        worker.await.unwrap().unwrap();
        match observed {
            Ok(result) => result.unwrap(),
            Err(_) => observation.await.unwrap(),
        }
        assert!(
            responsive,
            "credential work blocked session and app-lock observations"
        );
    }

    struct PausedValue {
        entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl Serialize for PausedValue {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            // Bincode may size a value before serializing it; pause only once.
            if let Some(entered) = self.entered.lock().unwrap().take() {
                entered.send(()).unwrap();
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
            serializer.serialize_str("replacement")
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn credential_storage_rechecks_session_and_lock_revision_after_preparation() {
        use tauri::Manager;
        for replace_session in [true, false] {
            let (root, app, node) = fixture();
            let manager = FileManager::new(root.path().to_path_buf());
            manager
                .write_encrypted_file("alice", "meta/value.enc", &"original", "pw")
                .unwrap();
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let value = PausedValue {
                entered: std::sync::Mutex::new(Some(entered_tx)),
                release: std::sync::Mutex::new(release_rx),
            };
            let worker_app = app.clone();
            let worker = tokio::spawn(async move {
                with_storage(&worker_app, &node, None, move |storage| {
                    storage.write("meta/value.enc", &value, "write failed")
                })
                .await
            });
            entered_rx.await.unwrap();
            if replace_session {
                // A same-user replacement must still invalidate the captured lease.
                let current = app.session().get().unwrap();
                app.session()
                    .set("replacement".into(), current.user, current.password);
            } else {
                let host = tauri::test::mock_builder()
                    .manage(app.clone())
                    .manage(app.app_lock().clone())
                    .build(tauri::test::mock_context(tauri::test::noop_assets()))
                    .unwrap();
                crate::app_lock::lock_app(host.state(), host.state()).unwrap();
                app.app_lock().clear();
            }
            release_tx.send(()).unwrap();
            assert!(matches!(
                worker.await.unwrap(),
                Err(CommandError::Authorization(_))
            ));
            assert_eq!(
                manager
                    .read_encrypted_file::<String>("alice", "meta/value.enc", "pw")
                    .unwrap(),
                "original"
            );
            assert_eq!(
                fs::read_dir(manager.user_data_path("alice").join("meta"))
                    .unwrap()
                    .count(),
                1
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn credential_storage_rejects_stale_plaintext_release_and_delete() {
        for delete in [false, true] {
            let (root, app, node) = fixture();
            let manager = FileManager::new(root.path().to_path_buf());
            manager
                .write_encrypted_file("alice", "meta/value.enc", &"secret", "pw")
                .unwrap();
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let worker_app = app.clone();
            let worker = tokio::spawn(async move {
                with_storage(&worker_app, &node, None, move |storage| {
                    let value: String = storage.read("meta/value.enc", "read failed")?;
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    if delete {
                        storage.delete("meta/value.enc", "delete failed")?;
                    }
                    Ok(value)
                })
                .await
            });
            entered_rx.await.unwrap();
            app.session().clear();
            release_tx.send(()).unwrap();
            assert!(matches!(
                worker.await.unwrap(),
                Err(CommandError::Authorization(_))
            ));
            assert!(manager.file_exists("alice", "meta/value.enc"));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn credential_storage_serializes_setup_changes_after_waiter_cancellation() {
        let (_root, app, node) = fixture();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first_app = app.clone();
        let first_node = node.clone();
        let first = tokio::spawn(async move {
            with_storage(&first_app, &first_node, None, move |storage| {
                let mut setup = load(storage)?;
                setup.confirmed = true;
                entered_tx.send(setup.recovery_key.clone()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                save(storage, &setup)
            })
            .await
        });
        let original_key = entered_rx.await.unwrap();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        let mut second = tokio::spawn(async move {
            with_storage(&app, &node, None, |storage| {
                let mut setup = load(storage)?;
                setup.destination = "drive".into();
                save(storage, &setup)?;
                Ok(setup)
            })
            .await
        });
        let completed_early = tokio::time::timeout(Duration::from_millis(100), &mut second)
            .await
            .is_ok();
        release_tx.send(()).unwrap();
        assert!(
            !completed_early,
            "cancelling the waiter released a live storage writer"
        );
        let setup = second.await.unwrap().unwrap();
        assert_eq!(setup.recovery_key, original_key);
        assert!(setup.confirmed);
        assert_eq!(setup.destination, "drive");
    }
}
