use super::{parse_channel_id, NodeState};
use crate::commands::CommandError;
use tauri::Manager;

fn file_kind(media: bool) -> mesh_talk_core::file::FileKind {
    if media {
        mesh_talk_core::file::FileKind::Media
    } else {
        mesh_talk_core::file::FileKind::File
    }
}

#[tauri::command]
pub async fn send_file_dm(
    app: tauri::AppHandle,
    state: tauri::State<'_, NodeState>,
    recipient: String,
    path: String,
    media: bool,
) -> Result<String, CommandError> {
    let node = state.node_handle().await?;
    // The per-file conv id isn't known until staging completes, but progress events key
    // on it — so the UI keys outgoing progress by the recipient until the id is returned
    // (it relabels on the resolved promise). Use the path-derived label up front.
    let mut prog = crate::events::ProgressThrottle::new(app, recipient.clone(), "send");
    let id = node
        .send_file_dm_progress(
            &recipient,
            std::path::Path::new(&path),
            file_kind(media),
            move |p| prog.emit(p.done, p.total),
        )
        .await
        .map_err(CommandError::from)?;
    Ok(hex::encode(id.as_bytes()))
}

#[tauri::command]
pub async fn send_file_to_account(
    app: tauri::AppHandle,
    state: tauri::State<'_, NodeState>,
    account: String,
    path: String,
    media: bool,
) -> Result<String, CommandError> {
    let node = state.node_handle().await?;
    let mut prog = crate::events::ProgressThrottle::new(app, account.clone(), "send");
    let id = node
        .send_file_to_account_progress(
            &account,
            std::path::Path::new(&path),
            file_kind(media),
            move |p| prog.emit(p.done, p.total),
        )
        .await
        .map_err(CommandError::from)?;
    Ok(hex::encode(id.as_bytes()))
}

#[tauri::command]
pub async fn send_file_channel(
    app: tauri::AppHandle,
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    path: String,
    media: bool,
) -> Result<String, CommandError> {
    let id = parse_channel_id(&channel_id)?;
    let node = state.node_handle().await?;
    let mut prog = crate::events::ProgressThrottle::new(app, channel_id.clone(), "send");
    let file_conv = node
        .send_file_channel_progress(
            id,
            std::path::Path::new(&path),
            file_kind(media),
            move |p| prog.emit(p.done, p.total),
        )
        .await
        .map_err(CommandError::from)?;
    Ok(hex::encode(file_conv.as_bytes()))
}

#[tauri::command]
pub async fn save_file(
    app: tauri::AppHandle,
    state: tauri::State<'_, NodeState>,
    file_conv: String,
    dest: String,
) -> Result<(), CommandError> {
    let id = parse_channel_id(&file_conv)?;
    let node = state.node_handle().await?;
    let mut prog = crate::events::ProgressThrottle::new(app, file_conv.clone(), "save");
    // save_file is synchronous (reads chunk events, decrypts, streams to disk) — run it
    // on a blocking thread so it doesn't stall the async runtime on a large file.
    tokio::task::spawn_blocking(move || {
        node.save_file_progress(id, std::path::Path::new(&dest), |p| {
            prog.emit(p.done, p.total)
        })
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
    .map_err(CommandError::from)
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

#[tauri::command]
pub async fn save_file_to_dir(
    state: tauri::State<'_, NodeState>,
    file_conv: String,
    dir: String,
) -> Result<String, CommandError> {
    let id = parse_channel_id(&file_conv)?;
    let node = state.node_handle().await?;
    let path = tokio::task::spawn_blocking(move || {
        node.save_file_into_dir(id, std::path::Path::new(&dir))
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
    .map_err(CommandError::from)?;
    Ok(path.to_string_lossy().into_owned())
}

/// Write raw bytes (e.g. an image pasted from the clipboard) to a temp file in the app
/// cache dir and return its path, so the caller can route it through the normal
/// file-send pipeline (which only takes a path). The name carries the given extension so
/// the received file is recognized as an image. Best-effort temp: it lives in the OS cache
/// dir and is overwritten on the next paste of the same name.
#[tauri::command]
pub async fn write_temp_file(
    app: tauri::AppHandle,
    bytes: Vec<u8>,
    ext: String,
    name: Option<String>,
) -> Result<String, CommandError> {
    // Bound a pasted image to a sane size so a paste can't write an arbitrarily large temp
    // file (the file pipeline enforces its own hard limit on the subsequent send).
    const MAX_PASTE_BYTES: usize = 64 * 1024 * 1024;
    if bytes.len() > MAX_PASTE_BYTES {
        return Err(CommandError::Validation("pasted image too large".into()));
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let basename = temp_file_basename(&ext, name.as_deref(), ts);
    // A unique per-write subdir, so keeping a REAL filename can't collide with (or overwrite)
    // another in-flight send of a same-named file — the displayed name is the basename.
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| CommandError::Validation(format!("no cache dir: {e}")))?
        .join("pasted")
        .join(ts.to_string());
    std::fs::create_dir_all(&dir).map_err(|e| CommandError::Validation(e.to_string()))?;
    let path = dir.join(basename);
    std::fs::write(&path, &bytes).map_err(|e| CommandError::Validation(e.to_string()))?;
    Ok(path.to_string_lossy().into_owned())
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
pub async fn read_file(
    state: tauri::State<'_, NodeState>,
    file_conv: String,
) -> Result<tauri::ipc::Response, CommandError> {
    let id = parse_channel_id(&file_conv)?;
    let node = state.node_handle().await?;
    let bytes = tokio::task::spawn_blocking(move || node.read_file(id))
        .await
        .map_err(|e| format!("join error: {e}"))?
        .map_err(CommandError::from)?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// Return DURABLE chat-media bytes (image/screenshot/video) from the media store for inline
/// preview. Distinct from `read_file`, which reassembles the transient chunks (gone after a
/// save/prune): media is copied to the store on send + receive-complete, so this survives
/// prune AND restart. Errors if no media is stored for `file_conv` (caller should not call
/// it for a generic attachment). Returned as a raw IPC response (ArrayBuffer in JS).
#[tauri::command]
pub async fn read_media(
    state: tauri::State<'_, NodeState>,
    file_conv: String,
) -> Result<tauri::ipc::Response, CommandError> {
    let id = parse_channel_id(&file_conv)?;
    let node = state.node_handle().await?;
    let bytes = tokio::task::spawn_blocking(move || node.read_media(id))
        .await
        .map_err(|e| format!("join error: {e}"))?
        .ok_or_else(|| CommandError::from("no stored media for this file".to_string()))?;
    Ok(tauri::ipc::Response::new(bytes))
}
