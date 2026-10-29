use super::{NodeState, parse_channel_id};
use crate::{commands::CommandError, runtime_admission::RuntimeAdmission, state::AppState};
use tauri::Manager;

fn file_kind(media: bool) -> mesh_talk_core::file::FileKind {
    if media {
        mesh_talk_core::file::FileKind::Media
    } else {
        mesh_talk_core::file::FileKind::File
    }
}

/// Session-local installed artwork can satisfy an independently authenticated sticker offer.
#[tauri::command]
pub async fn register_sticker_source(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    owner: String,
    account: String,
    name: String,
    bytes: Vec<u8>,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture(&app, &state, &owner)?;
    if bytes.len() > mesh_talk_core::file::sticker::MAX_STICKER_BYTES {
        return Err(CommandError::Validation("sticker source too large".into()));
    }
    admission
        .blocking(move |runtime, _| {
            let node = runtime.handle();
            if node.account_id() != account {
                return Err(CommandError::Authorization(
                    "runtime account mismatch".into(),
                ));
            }
            node.register_sticker_source(&name, &bytes)
                .map_err(CommandError::from)
        })
        .await
}

#[derive(serde::Serialize)]
pub struct FileStatus {
    file_conv: String,
    done: u32,
    total: u32,
    ready: bool,
    consent: Option<&'static str>,
}

/// One bounded snapshot for all visible attachments. The recipient's background chunk
/// sync does not emit IPC progress events, so the UI polls this cheap metadata endpoint.
#[tauri::command]
pub async fn file_statuses(
    state: tauri::State<'_, NodeState>,
    file_convs: Vec<String>,
) -> Result<Vec<FileStatus>, CommandError> {
    if file_convs.len() > 500 {
        return Err(CommandError::Validation("too many file statuses".into()));
    }
    let ids = file_convs
        .into_iter()
        .map(|file_conv| parse_channel_id(&file_conv).map(|id| (file_conv, id)))
        .collect::<Result<Vec<_>, _>>()?;
    let node = state.node_handle().await?;
    ids.into_iter()
        .map(|(file_conv, id)| {
            let progress = node.file_progress(id)?;
            let consent = node.attachment_consent_status(id)?;
            Ok(FileStatus {
                file_conv,
                done: progress.map_or(0, |p| p.done),
                total: progress.map_or(0, |p| p.total),
                ready: node.file_ready_to_save(id)?
                    && !matches!(
                        consent,
                        Some(
                            mesh_talk_core::node::ReceiveDecision::Pending
                                | mesh_talk_core::node::ReceiveDecision::Declined
                        )
                    ),
                consent: consent.map(|status| match status {
                    mesh_talk_core::node::ReceiveDecision::Pending => "pending",
                    mesh_talk_core::node::ReceiveDecision::Accepted => "accepted",
                    mesh_talk_core::node::ReceiveDecision::Declined => "declined",
                }),
            })
        })
        .collect::<Result<Vec<_>, CommandError>>()
}

#[tauri::command]
pub async fn media_migration_warning(
    state: tauri::State<'_, NodeState>,
) -> Result<bool, CommandError> {
    Ok(state.node_handle().await?.media_migration_warning())
}

#[derive(serde::Serialize)]
pub struct AttachmentReceivePolicy {
    global_auto: bool,
    contact_auto: Option<bool>,
}

#[tauri::command]
pub async fn attachment_receive_policy(
    state: tauri::State<'_, NodeState>,
    account: Option<String>,
) -> Result<AttachmentReceivePolicy, CommandError> {
    let node = state.node_handle().await?;
    Ok(AttachmentReceivePolicy {
        global_auto: node.attachment_global_auto(),
        contact_auto: account
            .as_deref()
            .and_then(|id| node.attachment_contact_auto(id)),
    })
}

#[tauri::command]
pub async fn set_attachment_global_auto(
    session: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    auto: bool,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&session, &state)?;
    admission
        .with_runtime(move |runtime| {
            runtime
                .handle()
                .set_attachment_global_auto(auto)
                .map_err(CommandError::from)
        })
        .await
}

#[tauri::command]
pub async fn set_attachment_contact_auto(
    session: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    account: String,
    auto: Option<bool>,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&session, &state)?;
    admission
        .with_runtime(move |runtime| {
            runtime
                .handle()
                .set_attachment_contact_auto(&account, auto)
                .map_err(CommandError::from)
        })
        .await
}

#[tauri::command]
pub async fn decide_attachment(
    session: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    file_conv: String,
    accept: bool,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&session, &state)?;
    admission
        .with_runtime(move |runtime| {
            let id = parse_channel_id(&file_conv)?;
            runtime
                .handle()
                .decide_attachment(id, accept)
                .map_err(CommandError::from)
        })
        .await
}

#[tauri::command]
pub async fn send_file_dm<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    session: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    recipient: String,
    path: String,
    media: bool,
) -> Result<String, CommandError> {
    let admission = RuntimeAdmission::capture_current(&session, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let node = runtime.handle();
                // The legacy DM command has no optimistic message key; retain its recipient key.
                let mut prog =
                    crate::events::ProgressThrottle::new(app.clone(), recipient.clone(), "prepare");
                let result = node
                    .send_file_dm_progress_if(
                        &recipient,
                        std::path::Path::new(&path),
                        file_kind(media),
                        move |p| prog.emit(p.done, p.total),
                        |commit| admission.authorize_accept(runtime, commit),
                    )
                    .await;
                cleanup_pasted_file(&app, &path);
                let id = result.map_err(CommandError::from)?;
                Ok(hex::encode(id.as_bytes()))
            })
        })
        .await
}

#[tauri::command]
pub async fn send_file_to_account<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    session: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    account: String,
    path: String,
    media: bool,
) -> Result<String, CommandError> {
    let admission = match RuntimeAdmission::capture_current(&session, &state) {
        Ok(admission) => admission,
        Err(error) => {
            cleanup_pasted_file(&app, &path);
            return Err(error);
        }
    };
    let mut prog = crate::events::ProgressThrottle::new(app.clone(), account.clone(), "prepare");
    let cleanup_app = app.clone();
    let cleanup_path = path.clone();
    let result = admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let result = runtime
                    .handle()
                    .send_file_to_account_progress_if(
                        &account,
                        std::path::Path::new(&path),
                        file_kind(media),
                        move |p| prog.emit(p.done, p.total),
                        |accept| admission.authorize_accept(runtime, accept),
                    )
                    .await;
                cleanup_pasted_file(&cleanup_app, &path);
                result
                    .map(|id| hex::encode(id.as_bytes()))
                    .map_err(CommandError::from)
            })
        })
        .await;
    // Also cover refusal before the detached operation obtains a runtime.
    if result.is_err() {
        cleanup_pasted_file(&app, &cleanup_path);
    }
    result
}

#[tauri::command]
pub async fn send_file_channel<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    session: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    path: String,
    media: bool,
    progress_key: Option<String>,
    expires_at_ms: Option<u64>,
) -> Result<String, CommandError> {
    let admission = RuntimeAdmission::capture_current(&session, &state)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let id = parse_channel_id(&channel_id)?;
                let node = runtime.handle();
                let mut prog = crate::events::ProgressThrottle::new(
                    app.clone(),
                    progress_key.unwrap_or_else(|| channel_id.clone()),
                    "prepare",
                );
                let result = node
                    .send_file_channel_progress_expiring_if(
                        id,
                        std::path::Path::new(&path),
                        file_kind(media),
                        move |p| prog.emit(p.done, p.total),
                        expires_at_ms,
                        |commit| admission.authorize_accept(runtime, commit),
                    )
                    .await;
                cleanup_pasted_file(&app, &path);
                let file_conv = result.map_err(CommandError::from)?;
                Ok(hex::encode(file_conv.as_bytes()))
            })
        })
        .await
}

#[tauri::command]
pub async fn save_file<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    session: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    file_conv: String,
    dest: String,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture_current(&session, &state)?;
    let id = parse_channel_id(&file_conv)?;
    let mut prog = crate::events::ProgressThrottle::new(app, file_conv, "save");
    admission
        .blocking(move |runtime, _| {
            runtime
                .handle()
                .save_file_progress(id, std::path::Path::new(&dest), |p| {
                    prog.emit(p.done, p.total)
                })
                .map_err(CommandError::from)
        })
        .await
}

/// Save a received file into a TRUSTED directory, deriving (and sanitizing) the
/// filename from the remote-supplied manifest name. The core strips directory
/// components, rejects traversal/absolute/drive prefixes, legalizes illegal chars, and
/// keeps the result inside `dir`, de-duplicating with a `name (N).ext` counter.
/// Returns the actual path written, so the UI can show where it landed.
/// The platform's standard Downloads folder — macOS `~/Downloads`, Windows the Downloads
/// known folder, Linux `XDG_DOWNLOAD_DIR` (from `~/.config/user-dirs.dirs`) falling back to
/// `~/Downloads` — resolved by Tauri's path API. This is the default save location when the
/// user hasn't chosen one. `None` only if no usable directory is resolvable at all.
#[tauri::command]
pub fn default_download_dir(app: tauri::AppHandle) -> Option<String> {
    use tauri::Manager;
    app.path()
        .download_dir()
        .ok()
        .or_else(|| crate::user_home_dir().map(|h| h.join("Downloads")))
        .map(|p| p.to_string_lossy().into_owned())
}

/// Open only the configured download folder (or the OS default) in the file manager.
/// The renderer cannot choose a different path for this command.
#[tauri::command]
pub fn open_download_dir(
    app: tauri::AppHandle,
    app_state: tauri::State<'_, crate::state::AppState>,
    settings: tauri::State<'_, crate::settings::SettingsState>,
    owner: String,
) -> Result<(), CommandError> {
    crate::config_store::with_account(&app_state, &owner, "settings.json", |_| {
        let configured = settings.get_for_owner(&owner)?.download_dir;
        let dir = if configured.is_empty() {
            default_download_dir(app)
                .ok_or_else(|| CommandError::Validation("download folder unavailable".into()))?
        } else {
            configured
        };
        let path = std::path::Path::new(&dir);
        if !path.is_dir() {
            return Err(CommandError::Validation(
                "download folder unavailable".into(),
            ));
        }
        #[cfg(target_os = "macos")]
        let opened = tauri_plugin_opener::open_path(path, Some("Finder"));
        #[cfg(not(target_os = "macos"))]
        let opened = tauri_plugin_opener::open_path(path, None::<&str>);
        opened.map_err(|error| CommandError::Service(format!("open download folder: {error}")))
    })
}

#[tauri::command]
pub async fn save_file_to_dir(
    session: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    file_conv: String,
    dir: String,
    expected_owner: Option<String>,
) -> Result<String, CommandError> {
    let id = parse_channel_id(&file_conv)?;
    if let Some(owner) = expected_owner {
        return save_file_to_dir_for_owner(
            session.inner().clone(),
            state.inner().clone(),
            &owner,
            id,
            dir,
        )
        .await;
    }
    let admission = RuntimeAdmission::capture_current(&session, &state)?;
    admission
        .blocking(move |runtime, _| {
            runtime
                .handle()
                .save_file_into_dir(id, std::path::Path::new(&dir))
                .map(|path| path.to_string_lossy().into_owned())
                .map_err(CommandError::from)
        })
        .await
}

async fn save_file_to_dir_for_owner(
    app: AppState,
    state: NodeState,
    owner: &str,
    id: mesh_talk_core::eventlog::ConversationId,
    dir: String,
) -> Result<String, CommandError> {
    // Capture before waiting, then retain runtime and session authority through
    // the actual disk write. A frontend lease check cannot protect native I/O.
    let admission = RuntimeAdmission::capture(&app, &state, owner)?;
    let guard = state.0.clone().read_owned().await;
    let path = tokio::task::spawn_blocking(move || {
        let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
        admission.checked(runtime, || {
            runtime
                .handle()
                .save_file_into_dir(id, std::path::Path::new(&dir))
                .map_err(CommandError::from)
        })
    })
    .await
    .map_err(|e| format!("join error: {e}"))??;
    Ok(path.to_string_lossy().into_owned())
}

/// Write raw bytes (e.g. an image pasted from the clipboard) to a temp file in the app
/// cache dir and return its path, so the caller can route it through the normal
/// file-send pipeline (which only takes a path). The name carries the given extension so
/// the received file is recognized as an image. Best-effort temp: it lives in the OS cache
/// dir and is overwritten on the next paste of the same name.
#[tauri::command]
pub async fn write_temp_file<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    session: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    bytes: Vec<u8>,
    ext: String,
    name: Option<String>,
) -> Result<String, CommandError> {
    let admission = RuntimeAdmission::capture_current(&session, &state)?;
    admission
        .blocking(move |_, _| {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let cache = app
                .path()
                .app_cache_dir()
                .map_err(|e| CommandError::Validation(format!("no cache dir: {e}")))?;
            let path = write_temp_file_in_cache(&cache, &bytes, &ext, name.as_deref(), ts)?;
            Ok(path.to_string_lossy().into_owned())
        })
        .await
}

fn write_temp_file_in_cache(
    cache: &std::path::Path,
    bytes: &[u8],
    ext: &str,
    name: Option<&str>,
    ts: u128,
) -> Result<std::path::PathBuf, CommandError> {
    // Bound a pasted image to a sane size so a paste can't write an arbitrarily large temp
    // file (the file pipeline enforces its own hard limit on the subsequent send).
    const MAX_PASTE_BYTES: usize = 64 * 1024 * 1024;
    if bytes.len() > MAX_PASTE_BYTES {
        return Err(CommandError::Validation("pasted image too large".into()));
    }
    let basename = temp_file_basename(ext, name, ts);
    // A unique per-write subdir, so keeping a REAL filename can't collide with (or overwrite)
    // another in-flight send of a same-named file — the displayed name is the basename.
    let root = cache.join("pasted");
    std::fs::create_dir_all(&root).map_err(|e| CommandError::Validation(e.to_string()))?;
    if !std::fs::symlink_metadata(&root)
        .map_err(|e| CommandError::Validation(e.to_string()))?
        .file_type()
        .is_dir()
    {
        return Err(CommandError::Validation(
            "pasted cache is not a directory".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| CommandError::Validation(e.to_string()))?;
    }
    let dir = root.join(format!("{ts}-{:016x}", rand::random::<u64>()));
    std::fs::create_dir(&dir).map_err(|e| CommandError::Validation(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| CommandError::Validation(e.to_string()))?;
    }
    let path = dir.join(basename);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    use std::io::Write;
    let mut file = options
        .open(&path)
        .map_err(|e| CommandError::Validation(e.to_string()))?;
    file.write_all(bytes)
        .map_err(|e| CommandError::Validation(e.to_string()))?;
    Ok(path)
}

pub(crate) fn cleanup_pasted_file<R: tauri::Runtime>(app: &tauri::AppHandle<R>, path: &str) {
    let Ok(cache) = app.path().app_cache_dir() else {
        return;
    };
    let root = cache.join("pasted");
    let source = std::path::Path::new(path);
    let Some(parent) = source.parent() else {
        return;
    };
    if parent.parent() != Some(root.as_path()) {
        return;
    }
    // Only our isolated staging dirs have the timestamp-random name.
    if !parent
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains('-'))
    {
        return;
    }
    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_dir(parent);
}

#[tauri::command]
pub fn delete_temp_file<R: tauri::Runtime>(app: tauri::AppHandle<R>, path: String) {
    cleanup_pasted_file(&app, &path);
}

pub(crate) fn clean_stale_pasted_files(cache: &std::path::Path) {
    let root = cache.join("pasted");
    if !std::fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Choose the on-disk name for a written temp file. With a real `original` filename (the
/// "send image/video" picker has one) we KEEP it — but only its basename, never a path
/// component — so the user's real name + extension survive to the chat list and the media
/// preview (a `.mov` stays `.mov`, not a MIME-derived `pasted-….quicktim`). For nameless
/// clipboard/screenshot bytes we synthesize `pasted-<ts>.<ext>`, sanitizing `ext` to a short
/// alphanumeric suffix (never trusted for a path).
pub(super) fn temp_file_basename(ext: &str, original: Option<&str>, ts: u128) -> String {
    if let Some(base) = original
        .and_then(|o| std::path::Path::new(o).file_name())
        .and_then(|s| s.to_str())
        .map(str::trim)
        .filter(|b| !b.is_empty() && *b != "." && *b != "..")
    {
        return base.to_string();
    }
    let ext: String = ext
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();
    let ext = if ext.is_empty() { "png" } else { &ext };
    format!("pasted-{ts}.{ext}")
}

/// Return a received file's decrypted bytes (for inline preview, e.g. images). Returned as a
/// raw IPC response (ArrayBuffer in JS) to avoid the overhead of a JSON number array.
#[tauri::command]
pub async fn read_file<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, NodeState>,
    file_conv: String,
) -> Result<tauri::ipc::Response, CommandError> {
    let admission = RuntimeAdmission::capture_current(app.state::<AppState>().inner(), &state)?;
    let id = parse_channel_id(&file_conv)?;
    let bytes = admission
        .clone()
        .blocking(move |runtime, _| runtime.handle().read_file(id).map_err(CommandError::from))
        .await?;
    admission
        .with_runtime(move |_| Ok(tauri::ipc::Response::new(bytes)))
        .await
}

/// Return DURABLE chat-media bytes (image/screenshot/video) from the media store for inline
/// preview. Distinct from `read_file`, which reassembles the transient chunks (gone after a
/// save/prune): media is copied to the store on send + receive-complete, so this survives
/// prune AND restart. Errors if no media is stored for `file_conv` (caller should not call
/// it for a generic attachment). Returned as a raw IPC response (ArrayBuffer in JS).
#[tauri::command]
pub async fn read_media<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, NodeState>,
    file_conv: String,
) -> Result<tauri::ipc::Response, CommandError> {
    let admission = RuntimeAdmission::capture_current(app.state::<AppState>().inner(), &state)?;
    let id = parse_channel_id(&file_conv)?;
    let bytes = admission
        .clone()
        .blocking(move |runtime, _| {
            runtime
                .handle()
                .read_media_checked(id)?
                .ok_or_else(|| CommandError::from("no stored media for this file".to_string()))
        })
        .await?;
    admission
        .with_runtime(move |_| Ok(tauri::ipc::Response::new(bytes)))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn automatic_save_rejects_another_account_before_waiting_for_its_runtime() {
        let root = tempfile::tempdir().unwrap();
        let app = AppState::new(crate::services::auth_service::AuthService::new(
            std::sync::Arc::new(mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
            )),
        ));
        let user = crate::services::user::User {
            user_id: "bob".into(),
            account_login_name: None,
            name: "bob".into(),
            display_name: "Bob".into(),
            address: "local".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        };
        app.session().publish("session".into(), user, "pw".into());
        let error = save_file_to_dir_for_owner(
            app,
            NodeState::empty(),
            "alice",
            mesh_talk_core::eventlog::ConversationId::new([1; 32]),
            root.path().join("downloads").to_string_lossy().into_owned(),
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("session owner mismatch"),
            "{error}"
        );
        assert!(!root.path().join("downloads").exists());
    }

    fn sticker_app() -> (tempfile::TempDir, tauri::App<tauri::test::MockRuntime>) {
        let root = tempfile::tempdir().unwrap();
        let app = AppState::new(crate::services::auth_service::AuthService::new(
            std::sync::Arc::new(mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
            )),
        ));
        app.session().publish(
            "session".into(),
            crate::services::user::User {
                user_id: "alice".into(),
                account_login_name: None,
                name: "alice".into(),
                display_name: "Alice".into(),
                address: "local".into(),
                created_at: 0,
                last_seen: 0,
                is_online: true,
            },
            "pw".into(),
        );
        let app = tauri::test::mock_builder()
            .manage(app)
            .manage(NodeState::empty())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        (root, app)
    }

    #[tokio::test]
    async fn registered_sticker_source_rejects_wrong_owner_before_waiting_for_runtime() {
        let (_root, app) = sticker_app();
        let state = app.state::<NodeState>();
        let _held = state.0.write().await;
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            register_sticker_source(
                app.state(),
                app.state(),
                "bob".into(),
                "account".into(),
                "sticker.png".into(),
                vec![],
            ),
        )
        .await
        .expect("wrong owner must not wait for runtime");
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("session owner mismatch")
        );
    }

    #[tokio::test]
    async fn registered_sticker_source_requires_current_crypto_account_and_valid_image() {
        let (root, app) = sticker_app();
        let runtime = mesh_talk_core::node::NodeRuntime::start(
            root.path(),
            "alice",
            "Alice",
            "pw",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        )
        .await
        .unwrap();
        let account = runtime.handle().account_id();
        *app.state::<NodeState>().0.write().await = Some(runtime);
        let wrong_account = register_sticker_source(
            app.state(),
            app.state(),
            "alice".into(),
            "another-account".into(),
            "sticker.png".into(),
            vec![],
        )
        .await
        .unwrap_err();
        assert!(
            wrong_account
                .to_string()
                .contains("runtime account mismatch"),
            "sticker registration must reject a different runtime account"
        );
        let name = "mt-sticker-v1-cGFjazpleGFtcGxlOndhdmUK8J-Riw.png";
        let bytes = hex::decode("89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d49444154789c63f8cfc0f01f00050001ff89993d1d0000000049454e44ae426082").unwrap();
        let valid = register_sticker_source(
            app.state(),
            app.state(),
            "alice".into(),
            account.clone(),
            name.into(),
            bytes,
        )
        .await;
        let invalid = register_sticker_source(
            app.state(),
            app.state(),
            "alice".into(),
            account.clone(),
            name.into(),
            b"not an image".to_vec(),
        )
        .await;
        let oversized = register_sticker_source(
            app.state(),
            app.state(),
            "alice".into(),
            account,
            name.into(),
            vec![0; mesh_talk_core::file::sticker::MAX_STICKER_BYTES + 1],
        )
        .await;
        app.state::<NodeState>()
            .0
            .write()
            .await
            .take()
            .unwrap()
            .stop()
            .await;
        valid.unwrap();
        assert!(invalid.is_err());
        assert!(oversized.is_err());
    }

    #[test]
    fn pasted_bytes_use_an_isolated_cache_path_and_reject_oversized_input() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = b"image bytes";
        let saved =
            write_temp_file_in_cache(cache.path(), bytes, "png", Some("photo.png"), 42).unwrap();
        assert!(saved.starts_with(cache.path()));
        assert_eq!(saved.file_name().unwrap(), "photo.png");
        assert_eq!(std::fs::read(&saved).unwrap(), bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&saved).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(saved.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        assert!(
            write_temp_file_in_cache(
                cache.path(),
                &vec![0; 64 * 1024 * 1024 + 1],
                "png",
                None,
                43
            )
            .is_err()
        );
    }
}
