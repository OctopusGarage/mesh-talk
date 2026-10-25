//! User-triggered Google Drive backup. OAuth and cloud I/O belong to the desktop host.
mod client;
mod oauth;
mod remembered;
use crate::{
    chat_commands::NodeState,
    commands::CommandError,
    recovery::{self, BackupCreated, BackupStatus, RestoreComplete},
    state::{AppState, SessionState},
};
use client::{DriveArchive, DriveClient};
use mesh_talk_core::{recovery::BackupPreview, storage::file_manager::FileManager};
use serde::Serialize;
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::watch;

#[derive(Default)]
pub(crate) struct DriveState {
    connections: Mutex<HashMap<String, Arc<Connection>>>,
    #[cfg(test)]
    client_override: Mutex<Option<DriveClient>>,
    #[cfg(test)]
    refresh_override: Mutex<Option<String>>,
}
struct Connection {
    generation: u64,
    session: SessionState,
    app_lock: crate::app_lock::AppLock,
    lock_revision: Option<u64>,
    cancelled: watch::Sender<bool>,
    busy: AtomicBool,
    token: tokio::sync::Mutex<Option<String>>,
    refresh_token: tokio::sync::Mutex<Option<String>>,
    token_expires: Mutex<Option<Instant>>,
    remembered_owner: Mutex<Option<remembered::Owner>>,
    #[cfg(test)]
    refresh_endpoint: Mutex<Option<String>>,
    archive: tokio::sync::Mutex<Option<Arc<tempfile::NamedTempFile>>>,
    progress: Mutex<DriveProgress>,
}
#[derive(Clone, Serialize)]
pub(crate) struct DriveProgress {
    phase: String,
    transferred: u64,
    total: u64,
}
impl Connection {
    fn new(session: SessionState, app_lock: crate::app_lock::AppLock) -> Self {
        let (cancelled, _) = watch::channel(false);
        Self {
            generation: session.generation(),
            session,
            lock_revision: app_lock.authorization_revision().ok(),
            app_lock,
            cancelled,
            busy: AtomicBool::new(false),
            token: Default::default(),
            refresh_token: Default::default(),
            token_expires: Default::default(),
            remembered_owner: Default::default(),
            #[cfg(test)]
            refresh_endpoint: Default::default(),
            archive: Default::default(),
            progress: Mutex::new(DriveProgress {
                phase: "connecting".into(),
                transferred: 0,
                total: 0,
            }),
        }
    }
    fn current(&self) -> Result<(), CommandError> {
        if self.lock_revision.is_none_or(|revision| {
            self.app_lock
                .with_unchanged_revision(revision, || Ok(()))
                .is_err()
        }) {
            self.cancelled.send_replace(true);
        }
        if *self.cancelled.borrow() {
            return Err(CommandError::Authorization(
                "Google Drive operation cancelled or expired. Reconnect Google Drive.".into(),
            ));
        }
        self.session
            .unchanged(self.generation, || ())
            .map_err(CommandError::Authorization)
    }
    fn phase(&self, phase: &str, transferred: u64, total: u64) {
        if let Ok(mut p) = self.progress.lock() {
            *p = DriveProgress {
                phase: phase.into(),
                transferred,
                total,
            };
        }
    }
    fn begin(self: &Arc<Self>) -> Result<Job, CommandError> {
        self.current()?;
        if self.busy.swap(true, Ordering::AcqRel) {
            return Err(CommandError::InvalidInput(
                "A Google Drive operation is already running".into(),
            ));
        }
        Ok(Job(self.clone()))
    }
    async fn run<T>(
        &self,
        future: impl Future<Output = Result<T, CommandError>>,
    ) -> Result<T, CommandError> {
        self.current()?;
        let mut cancelled = self.cancelled.subscribe();
        let mut timer = tokio::time::interval(Duration::from_millis(100));
        tokio::pin!(future);
        loop {
            tokio::select! {
                result = &mut future => { self.current()?; return result; },
                _ = cancelled.changed() => { self.current()?; },
                _ = timer.tick() => { self.current()?; },
            }
        }
    }
    // Cancellation must be observed during a snapshot, but the snapshot future
    // remains owned until its runtime-restart path has completed.
    async fn finish_snapshot<T>(
        &self,
        future: impl Future<Output = Result<T, CommandError>>,
    ) -> Result<T, CommandError> {
        let mut timer = tokio::time::interval(Duration::from_millis(100));
        let mut retired = None;
        tokio::pin!(future);
        loop {
            tokio::select! {
                result = &mut future => { return match retired { Some(error) => Err(error), None => { self.current()?; result } }; },
                _ = timer.tick() => { if let Err(error) = self.current() { retired = Some(error); } },
            }
        }
    }
    async fn refresh(&self, refresh_token: &str) -> Result<oauth::TokenResponse, CommandError> {
        #[cfg(test)]
        {
            let endpoint = self.refresh_endpoint.lock().unwrap().clone();
            if let Some(endpoint) = endpoint {
                return oauth::refresh_for_test(&endpoint, refresh_token).await;
            }
        }
        oauth::refresh(refresh_token).await
    }
    async fn install_token(
        self: &Arc<Self>,
        tokens: oauth::TokenResponse,
    ) -> Result<(), CommandError> {
        self.current()?;
        if let Some(refresh_token) = tokens.refresh_token {
            let owner = self.remembered_owner.lock().unwrap().clone();
            if let Some(owner) = owner {
                owner.save(refresh_token.clone(), self.clone()).await?;
            }
            *self.refresh_token.lock().await = Some(refresh_token);
        }
        self.current()?;
        *self.token.lock().await = Some(tokens.access_token);
        *self.token_expires.lock().unwrap() =
            Some(Instant::now() + Duration::from_secs(tokens.expires_in.saturating_sub(30)));
        Ok(())
    }
    async fn access_token(self: &Arc<Self>) -> Result<String, CommandError> {
        self.current()?;
        let expired = self
            .token_expires
            .lock()
            .unwrap()
            .is_some_and(|expiry| Instant::now() >= expiry);
        if expired {
            let refresh_token =
                self.refresh_token.lock().await.clone().ok_or_else(|| {
                    CommandError::Authentication("Connect Google Drive again".into())
                })?;
            let tokens = self.run(self.refresh(&refresh_token)).await?;
            self.install_token(tokens).await?;
        }
        self.token
            .lock()
            .await
            .clone()
            .ok_or_else(|| CommandError::Authentication("Connect Google Drive first".into()))
    }
}
struct Job(Arc<Connection>);
impl Drop for Job {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
    }
}
impl DriveState {
    fn insert(
        &self,
        id: &str,
        app: &AppState,
        node: &NodeState,
    ) -> Result<Arc<Connection>, CommandError> {
        if id.len() < 20
            || id.len() > 128
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(CommandError::InvalidInput(
                "Invalid Google Drive connection".into(),
            ));
        }
        let connection = Arc::new(Connection::new(
            app.session().clone(),
            app.app_lock().clone(),
        ));
        connection.current()?;
        let owner = remembered::Owner::capture(app, node);
        if owner
            .as_ref()
            .is_some_and(|owner| owner.generation() != connection.generation)
        {
            return Err(CommandError::Authorization(
                "Google Drive session was replaced".into(),
            ));
        }
        *connection.remembered_owner.lock().unwrap() = owner;
        #[cfg(test)]
        {
            *connection.refresh_endpoint.lock().unwrap() =
                self.refresh_override.lock().unwrap().clone();
        }
        let mut map = self
            .connections
            .lock()
            .map_err(|_| CommandError::Internal("Google Drive state unavailable".into()))?;
        map.retain(|_, item| item.current().is_ok());
        if map.contains_key(id) || map.len() >= 4 {
            return Err(CommandError::InvalidInput(
                "Close the existing Google Drive connection first".into(),
            ));
        }
        map.insert(id.into(), connection.clone());
        Ok(connection)
    }
    fn client(&self) -> Result<DriveClient, CommandError> {
        #[cfg(test)]
        if let Some(client) = self.client_override.lock().unwrap().as_ref() {
            return Ok(client.clone());
        }
        DriveClient::new()
    }
    fn get(&self, id: &str) -> Result<Arc<Connection>, CommandError> {
        let connection = self
            .connections
            .lock()
            .map_err(|_| CommandError::Internal("Google Drive state unavailable".into()))?
            .get(id)
            .cloned()
            .ok_or_else(|| CommandError::Authentication("Connect Google Drive first".into()))?;
        connection.current()?;
        Ok(connection)
    }
    // A cancelled OAuth attempt can finish after the view has reconnected with
    // the same ID. Its cleanup must never retire the replacement grant.
    fn remove_attempt(&self, id: &str, expected: &Arc<Connection>) {
        if let Ok(mut map) = self.connections.lock()
            && map
                .get(id)
                .is_some_and(|current| Arc::ptr_eq(current, expected))
            && let Some(connection) = map.remove(id)
        {
            connection.cancelled.send_replace(true);
        }
    }
    fn remove(&self, id: &str) {
        if let Ok(mut map) = self.connections.lock()
            && let Some(connection) = map.remove(id)
        {
            connection.cancelled.send_replace(true);
        }
    }
}
#[tauri::command]
pub(crate) fn google_drive_available() -> bool {
    oauth::configured()
}
#[tauri::command]
pub(crate) async fn google_drive_connect<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, DriveState>,
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, NodeState>,
    connection_id: String,
) -> Result<(), CommandError> {
    let connection = state.insert(&connection_id, &app_state, &node_state)?;
    let _job = connection.begin()?;
    let result = async {
        let tokens = connection.run(oauth::authorize(&app)).await?;
        if connection.remembered_owner.lock().unwrap().is_some() && tokens.refresh_token.is_none() {
            return Err(CommandError::Authentication("Google did not return a remembered connection. Connect Google Drive again and allow access.".into()));
        }
        connection.install_token(tokens).await?;
        connection.phase("connected", 0, 0);
        Ok(())
    }.await;
    if result.is_err() {
        state.remove_attempt(&connection_id, &connection);
    }
    result
}

/// Reopen only the signed-in local user's saved grant; never opens a browser.
#[tauri::command]
pub(crate) async fn google_drive_resume(
    state: tauri::State<'_, DriveState>,
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, NodeState>,
    connection_id: String,
) -> Result<bool, CommandError> {
    let Some(owner) = remembered::Owner::capture(&app_state, &node_state) else {
        return Ok(false);
    };
    let connection = state.insert(&connection_id, &app_state, &node_state)?;
    let _job = connection.begin()?;
    let result = async {
        let Some(refresh_token) = owner.load().await? else {
            return Ok(false);
        };
        connection.current()?;
        let tokens = match connection.run(connection.refresh(&refresh_token)).await {
            Ok(tokens) => tokens,
            Err(CommandError::Auth(_)) => {
                owner
                    .forget_rejected(refresh_token.clone(), connection.clone())
                    .await?;
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        *connection.refresh_token.lock().await = Some(refresh_token);
        connection.install_token(tokens).await?;
        connection.phase("connected", 0, 0);
        Ok(true)
    }
    .await;
    if !matches!(result, Ok(true)) {
        state.remove_attempt(&connection_id, &connection);
    }
    result
}

/// Explicitly forget remembered access. View cleanup uses disconnect instead.
#[tauri::command]
pub(crate) async fn google_drive_forget(
    state: tauri::State<'_, DriveState>,
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, NodeState>,
    connection_id: String,
) -> Result<(), CommandError> {
    let owner = remembered::Owner::capture(&app_state, &node_state);
    let generation = owner.as_ref().map_or_else(
        || app_state.session().generation(),
        remembered::Owner::generation,
    );
    {
        let mut map = state
            .connections
            .lock()
            .map_err(|_| CommandError::Internal("Google Drive state unavailable".into()))?;
        map.retain(|id, connection| {
            let keep = connection.generation != generation && id != &connection_id;
            if !keep {
                connection.cancelled.send_replace(true);
            }
            keep
        });
    }
    if let Some(owner) = owner {
        owner.forget().await?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn google_drive_disconnect(state: tauri::State<'_, DriveState>, connection_id: String) {
    state.remove(&connection_id);
}
#[tauri::command]
pub(crate) fn google_drive_progress(
    state: tauri::State<'_, DriveState>,
    connection_id: String,
) -> Result<DriveProgress, CommandError> {
    state
        .get(&connection_id)?
        .progress
        .lock()
        .map(|p| p.clone())
        .map_err(|_| CommandError::Internal("Google Drive progress unavailable".into()))
}
#[tauri::command]
pub(crate) async fn google_drive_list(
    state: tauri::State<'_, DriveState>,
    connection_id: String,
) -> Result<Vec<DriveArchive>, CommandError> {
    let connection = state.get(&connection_id)?;
    let _job = connection.begin()?;
    let token = connection.access_token().await?;
    connection.run(state.client()?.list(&token)).await
}
#[tauri::command]
pub(crate) async fn google_drive_create_backup<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, DriveState>,
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, NodeState>,
    connection_id: String,
    recovery_key: Option<String>,
    password: Option<String>,
) -> Result<BackupCreated, CommandError> {
    let connection = state.get(&connection_id)?;
    let job = connection.begin()?;
    connection.access_token().await?;
    let lease = app_state
        .session()
        .capture()
        .map_err(CommandError::Authentication)?;
    if lease.generation() != connection.generation {
        return Err(CommandError::Authorization(
            "Google Drive session was replaced".into(),
        ));
    }
    let recovery_key = match recovery_key {
        Some(key) => key,
        None => recovery::setup::saved_key(&app_state, &node_state, lease.generation()).await?,
    };
    let auth = app_state.inner().clone();
    let node = node_state.inner().clone();
    let client = state.client()?;
    let root = recovery::data_root(&node);
    // Own the scratch directory through snapshot completion, even if IPC is dropped
    // or the view disconnects. The existing snapshot worker must restart messaging.
    tokio::spawn(async move {
        let _job = job;
        let scratch = tempfile::tempdir()
            .map_err(|_| CommandError::Io("Could not stage Google Drive backup".into()))?;
        let path = scratch.path().join("archive.mtbackup");
        connection.phase("snapshot", 0, 0);
        let backup = connection
            .finish_snapshot(recovery::create_backup_with_status(
                app,
                &auth,
                &node,
                path.display().to_string(),
                recovery_key,
                false,
                Some(connection.generation),
                password,
            ))
            .await?;
        connection.current()?;
        let name = format!("mesh-talk-{}.mtbackup", backup.preview.created_at_ms);
        let token = connection.access_token().await?;
        let archive = connection
            .run(client.upload(&token, &path, &name, |sent, total| {
                connection.phase("uploading", sent, total)
            }))
            .await?;
        let status = BackupStatus {
            destination: format!("Google Drive / {}", archive.name),
            preview: backup.preview.clone(),
            archive_bytes: backup.archive_bytes,
        };
        let session = auth.session().clone();
        // A failed status update does not invalidate a complete remote archive.
        let _ = tokio::task::spawn_blocking(move || {
            session.matching(&lease, |info| {
                FileManager::new(root).write_encrypted_file(
                    &info.user.name,
                    "meta/backup_status.enc",
                    &status,
                    info.password.trim(),
                )
            })
        })
        .await;
        connection.current()?;
        connection.phase("complete", backup.archive_bytes, backup.archive_bytes);
        Ok(backup)
    })
    .await
    .map_err(|_| CommandError::Internal("Google Drive backup task stopped".into()))?
}
#[tauri::command]
pub(crate) async fn google_drive_download(
    state: tauri::State<'_, DriveState>,
    connection_id: String,
    file_id: String,
) -> Result<(), CommandError> {
    let connection = state.get(&connection_id)?;
    let _job = connection.begin()?;
    let token = connection.access_token().await?;
    *connection.archive.lock().await = None;
    connection.phase("downloading", 0, 0);
    let file = connection
        .run(
            state
                .client()?
                .download(&token, &file_id, |received, total| {
                    connection.phase("downloading", received, total)
                }),
        )
        .await?;
    *connection.archive.lock().await = Some(Arc::new(file));
    connection.phase("downloaded", 0, 0);
    Ok(())
}
async fn downloaded(connection: &Connection) -> Result<Arc<tempfile::NamedTempFile>, CommandError> {
    connection.current()?;
    connection.archive.lock().await.clone().ok_or_else(|| {
        CommandError::InvalidInput("Select and download a Google Drive backup first".into())
    })
}
#[tauri::command]
pub(crate) async fn google_drive_inspect(
    state: tauri::State<'_, DriveState>,
    connection_id: String,
    recovery_key: String,
) -> Result<BackupPreview, CommandError> {
    let connection = state.get(&connection_id)?;
    let job = connection.begin()?;
    let file = downloaded(&connection).await?;
    tokio::spawn(async move {
        let _job = job;
        let result =
            recovery::inspect_backup(file.path().display().to_string(), recovery_key).await?;
        connection.current()?;
        Ok(result)
    })
    .await
    .map_err(|_| CommandError::Internal("Google Drive inspection stopped".into()))?
}
#[tauri::command]
pub(crate) async fn google_drive_restore(
    state: tauri::State<'_, DriveState>,
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, NodeState>,
    connection_id: String,
    recovery_key: String,
    username: String,
    new_password: String,
) -> Result<RestoreComplete, CommandError> {
    let connection = state.get(&connection_id)?;
    let job = connection.begin()?;
    let file = downloaded(&connection).await?;
    // Once explicit restore is admitted, the existing transactional restore owns
    // publication; cancellation never interrupts a profile commit halfway through.
    let auth = app_state.inner().clone();
    let node = node_state.inner().clone();
    tokio::spawn(async move {
        let _job = job;
        recovery::restore_backup_with_generation(
            &auth,
            &node,
            file.path().display().to_string(),
            recovery_key,
            username,
            new_password,
            Some(connection.generation),
        )
        .await
    })
    .await
    .map_err(|_| CommandError::Internal("Google Drive restore stopped".into()))?
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unsampled_lock_revision_change_retires_an_idle_connection() {
        let app_lock = crate::app_lock::AppLock::default();
        let connection = Connection::new(SessionState::default(), app_lock.clone());
        assert!(connection.current().is_ok());
        // A completed lock/unlock cycle advances this authorization revision,
        // even when no cloud operation observed the intermediate locked state.
        app_lock.clear();
        assert!(!app_lock.is_locked());
        assert!(connection.current().is_err());
        let fresh = Connection::new(SessionState::default(), app_lock);
        assert!(fresh.current().is_ok());
    }
    #[tokio::test]
    async fn session_replacement_retires_pending_cloud_work() {
        let session = SessionState::default();
        let connection = Connection::new(session.clone(), crate::app_lock::AppLock::default());
        session.clear();
        assert!(connection.run(async { Ok(()) }).await.is_err());
    }
    #[tokio::test]
    async fn explicit_disconnect_cancels_work_and_releases_busy_guard() {
        let connection = Arc::new(Connection::new(
            SessionState::default(),
            crate::app_lock::AppLock::default(),
        ));
        let job = connection.begin().unwrap();
        assert!(connection.begin().is_err());
        connection.cancelled.send_replace(true);
        assert!(
            connection
                .run(std::future::pending::<Result<(), CommandError>>())
                .await
                .is_err()
        );
        drop(job);
        assert!(!connection.busy.load(Ordering::Acquire));
    }
    #[tokio::test]
    async fn cancellation_interrupts_an_already_pending_request() {
        let connection = Arc::new(Connection::new(
            SessionState::default(),
            crate::app_lock::AppLock::default(),
        ));
        let other = connection.clone();
        let task = tokio::spawn(async move {
            other
                .run(std::future::pending::<Result<(), CommandError>>())
                .await
        });
        tokio::task::yield_now().await;
        connection.cancelled.send_replace(true);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }
    #[tokio::test]
    async fn cancelled_snapshot_finishes_before_reporting_cancellation() {
        let connection = Arc::new(Connection::new(
            SessionState::default(),
            crate::app_lock::AppLock::default(),
        ));
        let (finished, wait) = tokio::sync::oneshot::channel();
        let other = connection.clone();
        let task = tokio::spawn(async move {
            other
                .finish_snapshot(async {
                    wait.await.unwrap();
                    Ok(())
                })
                .await
        });
        tokio::task::yield_now().await;
        connection.cancelled.send_replace(true);
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert!(!task.is_finished());
        finished.send(()).unwrap();
        assert!(task.await.unwrap().is_err());
    }
    #[test]
    fn drive_connection_rejects_authorization_revision_changes() {
        let lock = crate::app_lock::AppLock::default();
        let connection = Connection::new(SessionState::default(), lock.clone());
        assert!(connection.current().is_ok());
        lock.clear();
        assert!(!lock.is_locked());
        assert!(connection.current().is_err());
    }

    #[test]
    fn registered_drive_commands_return_structured_errors_and_release_state() {
        let app = tauri::test::mock_builder()
            .manage(DriveState::default())
            .invoke_handler(tauri::generate_handler![
                google_drive_available,
                google_drive_progress,
                google_drive_disconnect
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let view = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let invoke = |command: &str| {
            tauri::test::get_ipc_response(
                &view,
                tauri::webview::InvokeRequest {
                    cmd: command.into(),
                    callback: tauri::ipc::CallbackFn(0),
                    error: tauri::ipc::CallbackFn(1),
                    url: if cfg!(windows) {
                        "http://tauri.localhost"
                    } else {
                        "tauri://localhost"
                    }
                    .parse()
                    .unwrap(),
                    body: tauri::ipc::InvokeBody::Json(
                        serde_json::json!({"connectionId":"missing-connection-0000"}),
                    ),
                    headers: Default::default(),
                    invoke_key: tauri::test::INVOKE_KEY.into(),
                },
            )
            .map(|body| body.deserialize::<serde_json::Value>().unwrap())
        };
        assert_eq!(
            invoke("google_drive_available").unwrap(),
            serde_json::json!(oauth::configured())
        );
        assert_eq!(invoke("google_drive_progress").unwrap_err()["kind"], "auth");
        assert!(invoke("google_drive_disconnect").is_ok());
    }
    #[tokio::test]
    async fn stale_signed_out_generation_cannot_enter_profile_restore() {
        use crate::services::auth_service::AuthService;
        use mesh_talk_core::identity::manager::IdentityManager;
        let root = tempfile::tempdir().unwrap();
        let state = AppState::new(AuthService::new(Arc::new(IdentityManager::new(
            FileManager::new(root.path().to_path_buf()),
        ))));
        let generation = state.session().generation();
        state.session().clear();
        let result = recovery::restore_backup_with_generation(
            &state,
            &NodeState::empty(),
            "must-not-be-opened".into(),
            mesh_talk_core::recovery::key::RecoveryKey::generate().display(),
            "recovered".into(),
            "password123".into(),
            Some(generation),
        )
        .await;
        assert!(matches!(result, Err(CommandError::Authorization(_))));
        assert!(!root.path().join("users").exists());
    }
    #[test]
    fn retired_oauth_cleanup_cannot_disconnect_a_replacement_attempt() {
        let state = DriveState::default();
        let old = Arc::new(Connection::new(
            SessionState::default(),
            crate::app_lock::AppLock::default(),
        ));
        state
            .connections
            .lock()
            .unwrap()
            .insert("view".into(), old.clone());
        state.remove("view");
        let replacement = Arc::new(Connection::new(
            SessionState::default(),
            crate::app_lock::AppLock::default(),
        ));
        state
            .connections
            .lock()
            .unwrap()
            .insert("view".into(), replacement.clone());
        state.remove_attempt("view", &old);
        assert!(Arc::ptr_eq(&state.get("view").unwrap(), &replacement));
        assert!(replacement.current().is_ok());
        state.remove_attempt("view", &replacement);
        assert!(state.get("view").is_err());
    }
}

#[cfg(test)]
mod command_tests;
