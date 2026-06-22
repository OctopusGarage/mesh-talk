//! Tauri IPC commands backing the `/` route. They delegate to a
//! per-session [`NodeRuntime`] held in managed [`NodeState`] (populated on
//! login, cleared on logout). All are thin pass-throughs over the node API.

use crate::commands::CommandError;
use mesh_talk_core::eventlog::event::{ConversationId, EventId};
use mesh_talk_core::node::NodeRuntime;
use serde::Serialize;
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::Mutex;

/// Managed state holding the current session's node runtime (`None` until login).
#[derive(Clone)]
pub struct NodeState(pub Arc<Mutex<Option<NodeRuntime>>>);

impl NodeState {
    pub fn empty() -> Self {
        NodeState(Arc::new(Mutex::new(None)))
    }
}

impl Default for NodeState {
    fn default() -> Self {
        Self::empty()
    }
}

/// A peer as shown in the roster.
#[derive(Serialize)]
pub struct PeerInfo {
    pub user_id: String,
    pub name: String,
    pub addr: String,
    pub post_office: bool,
    /// The account this device belongs to (devices sharing it are one user's). The
    /// UI keys conversations by this so a multi-device contact is one conversation.
    pub account_id: Option<String>,
}

/// File metadata for a history line that represents a file/media message. Lets the UI
/// render inline media (image/video) or a file card, and read/save bytes by `file_conv`.
#[derive(Serialize)]
pub struct HistoryFileInfo {
    pub file_conv: String, // hex — pass to read_file/save_file
    pub name: String,
    pub size: u64,
    pub mime: String,
}

/// One merged history line (sent or received) for display.
#[derive(Serialize)]
pub struct HistoryItem {
    pub id: Option<String>, // hex EventId; null when there is no stable id (see From impl)
    pub from_me: bool,
    pub who: String,
    pub text: String,
    pub wall_clock: u64,
    pub reply_to: Option<String>, // hex EventId of the parent message, if any
    pub file: Option<HistoryFileInfo>, // present when this line is a file/media message
}

impl From<mesh_talk_core::node::HistoryEntry> for HistoryItem {
    fn from(h: mesh_talk_core::node::HistoryEntry) -> Self {
        // A sent entry whose event isn't yet in the log gets the all-zero sentinel id; surface
        // it as null (like a pending message) so the UI never targets a react/reply at a
        // bogus id, instead of leaking the sentinel as a real hex id.
        let id = if h.id.as_bytes() == &[0u8; 32] {
            None
        } else {
            Some(hex::encode(h.id.as_bytes()))
        };
        HistoryItem {
            id,
            from_me: h.from_me,
            who: h.who,
            text: String::from_utf8_lossy(&h.text).into_owned(),
            wall_clock: h.wall_clock,
            reply_to: h.reply_to.map(|id| hex::encode(id.as_bytes())),
            file: h.file.map(|f| HistoryFileInfo {
                file_conv: hex::encode(f.file_conv.as_bytes()),
                name: f.name,
                size: f.size,
                mime: f.mime,
            }),
        }
    }
}

/// Aggregated reaction for display.
#[derive(Serialize)]
pub struct ReactionInfo {
    pub target: String, // hex EventId
    pub emoji: String,
    pub who: Vec<String>,
}

/// A channel member as shown in the chat UI.
#[derive(Serialize)]
pub struct ChannelMemberInfo {
    pub user_id: String,
    pub name: String,
}

/// A channel's membership plus its owner. The owner is the only principal allowed to
/// change membership (enforced in core); the UI uses it to show the owner badge and to
/// reveal the add/remove controls only to the owner.
#[derive(Serialize)]
pub struct ChannelMembersInfo {
    pub owner: String,
    pub members: Vec<ChannelMemberInfo>,
}

#[tauri::command]
pub async fn my_id(state: tauri::State<'_, NodeState>) -> Result<String, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt.user_id().to_string())
}

#[tauri::command]
pub async fn list_peers(state: tauri::State<'_, NodeState>) -> Result<Vec<PeerInfo>, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt
        .peers()
        .into_iter()
        .map(|p| PeerInfo {
            user_id: p.public.user_id(),
            name: p.name,
            addr: p.addr.to_string(),
            post_office: p.post_office,
            account_id: p.account_id,
        })
        .collect())
}

#[tauri::command]
pub async fn send_dm(
    state: tauri::State<'_, NodeState>,
    recipient: String,
    text: String,
    reply_to: Option<String>,
) -> Result<(), CommandError> {
    // Snapshot the node handle, then release the state lock before the .await send.
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        rt.handle()
    };
    let reply = match reply_to {
        Some(h) => Some(parse_event_id(&h)?),
        None => None,
    };
    node.send_dm_reply(&recipient, text.as_bytes(), reply)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn history(
    state: tauri::State<'_, NodeState>,
    peer: String,
    limit: usize,
) -> Result<Vec<HistoryItem>, CommandError> {
    // Cap the page size so a frontend accident (e.g. a huge JS number) can't
    // request an unbounded scan; the node truncates to this anyway.
    let limit = limit.min(500);
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    let public = rt
        .peer_public(&peer)
        .ok_or_else(|| CommandError::Validation(format!("unknown peer: {peer}")))?;
    Ok(rt
        .history(&public, limit)
        .into_iter()
        .map(HistoryItem::from)
        .collect())
}

#[tauri::command]
pub async fn account_id(state: tauri::State<'_, NodeState>) -> Result<String, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt.account_id().to_string())
}

/// Publish (or clear) this user's OWN avatar to peers as a signed profile. Called by the
/// frontend when the user sets/removes their own photo (the local `avatars.json` mirror is
/// still written by `set_avatar` so the override-precedence logic is unchanged). `avatar`
/// is the small data-URL string; `None` clears it (propagates a "no avatar"). The node
/// bounds the size and signs it with the account key.
#[tauri::command]
pub async fn publish_avatar(
    state: tauri::State<'_, NodeState>,
    avatar: Option<String>,
) -> Result<(), CommandError> {
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        rt.handle()
    };
    node.set_avatar(avatar.map(|s| s.into_bytes()))
        .await
        .map_err(CommandError::from)
}

/// Every avatar peers have propagated to us, as `account_id -> data-URL`. The frontend
/// merges these into its avatars store on startup so received avatars survive a relaunch.
#[tauri::command]
pub async fn peer_avatars(
    state: tauri::State<'_, NodeState>,
) -> Result<std::collections::HashMap<String, String>, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt
        .peer_avatars()
        .into_iter()
        .map(|(id, bytes)| (id, String::from_utf8_lossy(&bytes).into_owned()))
        .collect())
}

#[tauri::command]
pub async fn send_to_account(
    state: tauri::State<'_, NodeState>,
    account: String,
    text: String,
    reply_to: Option<String>,
) -> Result<(), CommandError> {
    // Snapshot the node handle, then release the state lock before the .await send.
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        rt.handle()
    };
    let reply = match reply_to {
        Some(h) => Some(parse_event_id(&h)?),
        None => None,
    };
    node.send_to_account(&account, text.as_bytes(), reply)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn account_history(
    state: tauri::State<'_, NodeState>,
    account: String,
    limit: usize,
) -> Result<Vec<HistoryItem>, CommandError> {
    let limit = limit.min(500);
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt
        .account_history(&account, limit)
        .into_iter()
        .map(HistoryItem::from)
        .collect())
}

#[tauri::command]
pub async fn start_linking(state: tauri::State<'_, NodeState>) -> Result<String, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt.start_linking())
}

#[tauri::command]
pub async fn stop_linking(state: tauri::State<'_, NodeState>) -> Result<(), CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    rt.stop_linking();
    Ok(())
}

#[tauri::command]
pub async fn link_device(
    state: tauri::State<'_, NodeState>,
    peer: String,
    code: String,
) -> Result<String, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    rt.link_device(&peer, &code)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn rekey_account(state: tauri::State<'_, NodeState>) -> Result<String, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    rt.rekey_account().map_err(CommandError::from)
}

/// An account (group of devices) as shown in the chat UI.
#[derive(Serialize)]
pub struct AccountInfo {
    pub account_id: String,
    pub device_count: usize,
    pub names: Vec<String>,
}

#[tauri::command]
pub async fn list_accounts(
    state: tauri::State<'_, NodeState>,
) -> Result<Vec<AccountInfo>, CommandError> {
    use std::collections::BTreeMap;
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    let mut by_account: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for p in rt.peers() {
        if let Some(acct) = p.account_id {
            by_account.entry(acct).or_default().push(p.name);
        }
    }
    Ok(by_account
        .into_iter()
        .map(|(account_id, names)| AccountInfo {
            device_count: names.len(),
            names,
            account_id,
        })
        .collect())
}

/// A channel as shown in the chat UI.
#[derive(Serialize)]
pub struct ChannelInfo {
    pub channel_id: String, // hex
    pub name: String,
    pub member_count: usize,
}

fn parse_channel_id(hex_id: &str) -> Result<ConversationId, CommandError> {
    let bytes =
        hex::decode(hex_id).map_err(|_| CommandError::Validation("invalid channel id".into()))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| CommandError::Validation("channel id must be 32 bytes".into()))?;
    Ok(ConversationId::new(arr))
}

fn parse_event_id(hex_id: &str) -> Result<EventId, CommandError> {
    let bytes =
        hex::decode(hex_id).map_err(|_| CommandError::Validation("invalid event id".into()))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| CommandError::Validation("event id must be 32 bytes".into()))?;
    Ok(EventId::new(arr))
}

fn to_reaction_infos(views: Vec<mesh_talk_core::node::ReactionView>) -> Vec<ReactionInfo> {
    views
        .into_iter()
        .map(|v| ReactionInfo {
            target: v.target,
            emoji: v.emoji,
            who: v.who,
        })
        .collect()
}

#[tauri::command]
pub async fn list_channels(
    state: tauri::State<'_, NodeState>,
) -> Result<Vec<ChannelInfo>, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt
        .list_channels()
        .into_iter()
        .map(|c| ChannelInfo {
            channel_id: hex::encode(c.id.as_bytes()),
            name: c.name,
            member_count: c.member_count,
        })
        .collect())
}

#[tauri::command]
pub async fn channel_members(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
) -> Result<ChannelMembersInfo, CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    // Build a user_id -> display name map from the known roster so members show a
    // friendly name when we know one; otherwise fall back to the user_id.
    let mut names: std::collections::HashMap<String, String> = rt
        .peers()
        .into_iter()
        .map(|p| (p.public.user_id(), p.name))
        .collect();
    // We are never in our own discovery roster, so add ourselves explicitly — otherwise
    // the self member falls back to its raw user_id (hex) instead of our display name.
    names.insert(rt.user_id().to_string(), rt.display_name().to_string());
    let members = rt
        .channel_members(channel)
        .into_iter()
        .map(|p| {
            let user_id = p.user_id();
            let name = names
                .get(&user_id)
                .cloned()
                .unwrap_or_else(|| user_id.clone());
            ChannelMemberInfo { user_id, name }
        })
        .collect();
    Ok(ChannelMembersInfo {
        owner: rt.channel_owner(channel),
        members,
    })
}

#[tauri::command]
pub async fn create_channel(
    state: tauri::State<'_, NodeState>,
    name: String,
    member_ids: Vec<String>,
) -> Result<String, CommandError> {
    let (node, members) = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        let mut members = Vec::new();
        for uid in &member_ids {
            let p = rt
                .peer_public(uid)
                .ok_or_else(|| CommandError::Validation(format!("unknown peer: {uid}")))?;
            members.push(p);
        }
        (rt.handle(), members)
    };
    let id = node
        .create_channel(&name, members)
        .await
        .map_err(CommandError::from)?;
    Ok(hex::encode(id.as_bytes()))
}

#[tauri::command]
pub async fn add_channel_member(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    member_id: String,
) -> Result<(), CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let (node, member) = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        let member = rt
            .peer_public(&member_id)
            .ok_or_else(|| CommandError::Validation(format!("unknown peer: {member_id}")))?;
        (rt.handle(), member)
    };
    node.add_channel_member(channel, member)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn remove_channel_member(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    member_id: String,
) -> Result<(), CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        rt.handle()
    };
    node.remove_channel_member(channel, &member_id)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn send_channel_message(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    text: String,
    reply_to: Option<String>,
) -> Result<(), CommandError> {
    let id = parse_channel_id(&channel_id)?;
    let reply = match reply_to {
        Some(h) => Some(parse_event_id(&h)?),
        None => None,
    };
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        rt.handle()
    };
    node.send_channel_message_reply(id, text.as_bytes(), reply)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn send_file_dm(
    app: tauri::AppHandle,
    state: tauri::State<'_, NodeState>,
    recipient: String,
    path: String,
) -> Result<String, CommandError> {
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        rt.handle()
    };
    // The per-file conv id isn't known until staging completes, but progress events key
    // on it — so the UI keys outgoing progress by the recipient until the id is returned
    // (it relabels on the resolved promise). Use the path-derived label up front.
    let mut prog = crate::events::ProgressThrottle::new(app, recipient.clone(), "send");
    let id = node
        .send_file_dm_progress(&recipient, std::path::Path::new(&path), move |p| {
            prog.emit(p.done, p.total)
        })
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
) -> Result<String, CommandError> {
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        rt.handle()
    };
    let mut prog = crate::events::ProgressThrottle::new(app, account.clone(), "send");
    let id = node
        .send_file_to_account_progress(&account, std::path::Path::new(&path), move |p| {
            prog.emit(p.done, p.total)
        })
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
) -> Result<String, CommandError> {
    let id = parse_channel_id(&channel_id)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        rt.handle()
    };
    let mut prog = crate::events::ProgressThrottle::new(app, channel_id.clone(), "send");
    let file_conv = node
        .send_file_channel_progress(id, std::path::Path::new(&path), move |p| {
            prog.emit(p.done, p.total)
        })
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
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        rt.handle()
    };
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
#[tauri::command]
pub async fn save_file_to_dir(
    state: tauri::State<'_, NodeState>,
    file_conv: String,
    dir: String,
) -> Result<String, CommandError> {
    let id = parse_channel_id(&file_conv)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        rt.handle()
    };
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
) -> Result<String, CommandError> {
    // Bound a pasted image to a sane size so a paste can't write an arbitrarily large temp
    // file (the file pipeline enforces its own hard limit on the subsequent send).
    const MAX_PASTE_BYTES: usize = 64 * 1024 * 1024;
    if bytes.len() > MAX_PASTE_BYTES {
        return Err(CommandError::Validation("pasted image too large".into()));
    }
    // Sanitize the extension to a short alphanumeric suffix (never trust it for a path).
    let ext: String = ext
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();
    let ext = if ext.is_empty() {
        "png".to_string()
    } else {
        ext
    };
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| CommandError::Validation(format!("no cache dir: {e}")))?
        .join("pasted");
    std::fs::create_dir_all(&dir).map_err(|e| CommandError::Validation(e.to_string()))?;
    let name = format!(
        "pasted-{}.{ext}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );
    let path = dir.join(name);
    std::fs::write(&path, &bytes).map_err(|e| CommandError::Validation(e.to_string()))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Capture a screenshot and return it as PNG bytes, for sending as an inline image.
///
/// `hide_window`: when true, the main app window is hidden before the capture (so the app
/// itself isn't in the shot — WeChat/QQ style), then shown + focused again afterwards. The
/// window is ALWAYS restored, even when the capture errors or the user cancels.
///
/// Returns:
/// - non-empty PNG bytes on a successful capture,
/// - `Ok(vec![])` (empty) when the user cancels the capture (frontend then sends nothing),
/// - `Err(CommandError)` on a real failure (e.g. missing permission) so the UI can prompt.
///
/// Per-platform capture mechanism:
/// - macOS: shells out to the built-in `screencapture -i` interactive region/window
///   selector, which blocks until the user selects an area or presses Esc.
///   NOTE: macOS screen capture requires the "Screen Recording" permission (TCC). If it is
///   not granted, the produced PNG is blank/empty; the user must grant it in
///   System Settings → Privacy & Security → Screen Recording.
/// - Windows/Linux: not wired up yet — returns a clear error (cross-platform capture is a
///   documented follow-up; see `capture_png`).
/// Set the OS app-icon unread badge: the macOS dock number, the Windows taskbar overlay,
/// or the Linux Unity launcher count. `count` is the total unread messages; 0 (or None)
/// clears the badge. Best-effort — a platform without badge support just no-ops.
#[tauri::command]
pub async fn set_badge(app: tauri::AppHandle, count: u32) -> Result<(), CommandError> {
    if let Some(window) = app.get_webview_window("main") {
        let n = if count == 0 { None } else { Some(count as i64) };
        let _ = window.set_badge_count(n);
    }
    Ok(())
}

#[tauri::command]
pub async fn capture_screen(
    app: tauri::AppHandle,
    hide_window: bool,
) -> Result<Vec<u8>, CommandError> {
    let window = app.get_webview_window("main");

    if hide_window {
        if let Some(w) = &window {
            let _ = w.hide();
        }
        // Give the compositor a moment to actually remove the window from the screen before
        // we capture, otherwise it can still be in the shot.
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
    }

    let result = capture_png().await;

    if hide_window {
        if let Some(w) = &window {
            let _ = w.show();
            let _ = w.set_focus();
        }
    }

    result
}

/// Platform-specific capture, returning PNG bytes (empty = user cancelled).
/// macOS Screen Recording permission (TCC) check + request, via CoreGraphics. Without this
/// permission, `screencapture` silently returns only the DESKTOP wallpaper (window content is
/// blanked) — so we must verify it up front rather than hand back a confusing desktop shot.
#[cfg(target_os = "macos")]
mod screen_recording {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }
    /// Whether this app currently holds the Screen Recording permission.
    pub fn has_access() -> bool {
        unsafe { CGPreflightScreenCaptureAccess() }
    }
    /// Register the app in (and prompt for) System Settings → Screen Recording. A fresh
    /// grant only takes effect for capture after the app is restarted.
    pub fn request_access() {
        unsafe {
            let _ = CGRequestScreenCaptureAccess();
        }
    }
}

/// Sentinel error message the frontend matches to show the "grant Screen Recording" hint.
#[cfg(target_os = "macos")]
const SCREEN_PERMISSION_ERR: &str = "screen-recording-permission";

#[cfg(target_os = "macos")]
async fn capture_png() -> Result<Vec<u8>, CommandError> {
    // Without the Screen Recording permission, screencapture would just grab the desktop
    // wallpaper. Verify first; if missing, prompt/register the app and bail with a clear
    // signal so the UI tells the user to grant it (and restart) — not a desktop screenshot.
    if !screen_recording::has_access() {
        screen_recording::request_access();
        return Err(CommandError::Internal(SCREEN_PERMISSION_ERR.into()));
    }
    // Interactive selector: `-i` lets the user drag a region or pick a window; Esc cancels.
    // It writes a PNG to the given path only if the user actually selects something.
    let tmp = std::env::temp_dir().join(format!(
        "mesh-talk-shot-{}.png",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let tmp_clone = tmp.clone();
    let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, CommandError> {
        let status = std::process::Command::new("screencapture")
            .arg("-i")
            .arg(&tmp_clone)
            .status()
            .map_err(|e| CommandError::Internal(format!("screencapture failed: {e}")))?;
        if !status.success() {
            // The user pressed Esc / cancelled — no file, nothing to send.
            return Ok(Vec::new());
        }
        match std::fs::read(&tmp_clone) {
            // Cancel can also exit 0 without writing the file.
            Err(_) => Ok(Vec::new()),
            Ok(b) => {
                let _ = std::fs::remove_file(&tmp_clone);
                Ok(b)
            }
        }
    })
    .await
    .map_err(|e| CommandError::Internal(format!("join error: {e}")))??;
    Ok(bytes)
}

/// Windows/Linux: screenshot capture isn't wired up yet (the macOS path uses the native
/// `screencapture` selector). Returning a clear error keeps the build dependency-free — a
/// cross-platform capture crate (e.g. `xcap`) pulls in extra system libraries (libxcb,
/// libdbus) that CI's Linux/Windows build steps don't install, so wiring it up (with the
/// matching CI apt packages + region selection) is a documented follow-up.
#[cfg(not(target_os = "macos"))]
async fn capture_png() -> Result<Vec<u8>, CommandError> {
    Err(CommandError::Internal(
        "screenshot capture is currently only supported on macOS".into(),
    ))
}

/// A fingerprint rendered for human comparison: the same fingerprint grouped into
/// readable blocks plus a short deterministic word sequence. Pure presentation of the
/// EXISTING fingerprint (no crypto), so the UI can show a "safety number" the user
/// compares out-of-band.
#[derive(Serialize)]
pub struct SafetyNumber {
    pub grouped: String,
    pub words: Vec<String>,
}

/// Compute the safety-number rendering of a fingerprint. Stateless + synchronous.
#[tauri::command]
pub fn safety_number(fingerprint: String) -> SafetyNumber {
    SafetyNumber {
        grouped: mesh_talk_core::util::safety_number::grouped(&fingerprint),
        words: mesh_talk_core::util::safety_number::words(&fingerprint, 4)
            .into_iter()
            .map(str::to_string)
            .collect(),
    }
}

/// Return a received file's decrypted bytes (for inline preview, e.g. images). Returned as a
/// raw IPC response (ArrayBuffer in JS) to avoid the overhead of a JSON number array.
#[tauri::command]
pub async fn read_file(
    state: tauri::State<'_, NodeState>,
    file_conv: String,
) -> Result<tauri::ipc::Response, CommandError> {
    let id = parse_channel_id(&file_conv)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        rt.handle()
    };
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
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        rt.handle()
    };
    let bytes = tokio::task::spawn_blocking(move || node.read_media(id))
        .await
        .map_err(|e| format!("join error: {e}"))?
        .ok_or_else(|| CommandError::from("no stored media for this file".to_string()))?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
pub async fn channel_history(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    limit: usize,
) -> Result<Vec<HistoryItem>, CommandError> {
    let limit = limit.min(500);
    let id = parse_channel_id(&channel_id)?;
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt
        .channel_history(id, limit)
        .into_iter()
        .map(HistoryItem::from)
        .collect())
}

#[tauri::command]
pub async fn react_dm(
    state: tauri::State<'_, NodeState>,
    recipient: String,
    target: String,
    emoji: String,
    remove: bool,
) -> Result<(), CommandError> {
    let id = parse_event_id(&target)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        rt.handle()
    };
    node.react_dm(&recipient, id, &emoji, remove)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn react_channel(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    target: String,
    emoji: String,
    remove: bool,
) -> Result<(), CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let id = parse_event_id(&target)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        rt.handle()
    };
    node.react_channel(channel, id, &emoji, remove)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn reactions(
    state: tauri::State<'_, NodeState>,
    peer: String,
) -> Result<Vec<ReactionInfo>, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    let public = rt
        .peer_public(&peer)
        .ok_or_else(|| CommandError::Validation(format!("unknown peer: {peer}")))?;
    Ok(to_reaction_infos(rt.reactions_dm(&public)))
}

#[tauri::command]
pub async fn channel_reactions(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
) -> Result<Vec<ReactionInfo>, CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(to_reaction_infos(rt.channel_reactions(channel)))
}

#[tauri::command]
pub async fn react_account(
    state: tauri::State<'_, NodeState>,
    account: String,
    target: String,
    emoji: String,
    remove: bool,
) -> Result<(), CommandError> {
    let id = parse_event_id(&target)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        rt.handle()
    };
    node.react_to_account(&account, id, &emoji, remove)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn account_reactions(
    state: tauri::State<'_, NodeState>,
    account: String,
) -> Result<Vec<ReactionInfo>, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(to_reaction_infos(rt.account_reactions(&account)))
}

/// A search result hit for display in the UI.
#[derive(Serialize)]
pub struct SearchHitInfo {
    pub is_channel: bool,
    pub target: String,
    pub label: String,
    pub from_me: bool,
    pub who: String,
    pub text: String,
    pub wall_clock: u64,
}

#[tauri::command]
pub async fn search(
    state: tauri::State<'_, NodeState>,
    query: String,
) -> Result<Vec<SearchHitInfo>, CommandError> {
    // Snapshot the node handle and DROP the state lock before the scan, so a search can't
    // serialize all other node IPC behind the synchronous scan; run the scan on the
    // blocking pool (it reads/decrypts every conversation's stores).
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
        rt.handle()
    };
    let hits = tokio::task::spawn_blocking(move || node.search(&query))
        .await
        .map_err(|e| format!("join error: {e}"))?;
    Ok(hits
        .into_iter()
        .map(|h| SearchHitInfo {
            is_channel: h.is_channel,
            target: h.target,
            label: h.label,
            from_me: h.from_me,
            who: h.who,
            text: String::from_utf8_lossy(&h.text).into_owned(),
            wall_clock: h.wall_clock,
        })
        .collect())
}

// --- Diagnostics / discovery ------------------------------------------------

/// A discovered peer as shown on the Diagnostics page.
#[derive(Serialize)]
pub struct DiagPeerInfo {
    pub user_id: String,
    pub name: String,
    pub ip: String,
    pub tcp_port: u16,
    pub post_office: bool,
    pub account_id: Option<String>,
    /// Whole seconds since this peer was last heard from.
    pub last_seen_secs: u64,
}

/// This device's own identity + network facts, for the Diagnostics page.
#[derive(Serialize)]
pub struct DiagNetworkInfo {
    pub own_user_id: String,
    pub own_name: String,
    pub account_id: String,
    pub listen_tcp_port: u16,
    pub discovery_port: u16,
    pub multicast_group: String,
    pub interfaces: Vec<String>,
}

/// Snapshot the current roster for the Diagnostics page. Polled by the frontend.
#[tauri::command]
pub async fn diag_get_peers(
    state: tauri::State<'_, NodeState>,
) -> Result<Vec<DiagPeerInfo>, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(rt
        .peers()
        .into_iter()
        .map(|p| DiagPeerInfo {
            user_id: p.public.user_id(),
            name: p.name,
            ip: p.addr.ip().to_string(),
            tcp_port: p.addr.port(),
            post_office: p.post_office,
            account_id: p.account_id,
            last_seen_secs: p.last_seen.elapsed().as_secs(),
        })
        .collect())
}

/// Force an immediate re-announce + /24 rescan (the manual "announce now" control on
/// the Diagnostics page). Helps converge first-contact when LAN discovery is flaky.
#[tauri::command]
pub async fn rescan_peers(state: tauri::State<'_, NodeState>) -> Result<(), CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    rt.trigger_discovery();
    Ok(())
}

/// This device's own identity + LAN/discovery facts, for the Diagnostics page.
#[tauri::command]
pub async fn diag_network_info(
    state: tauri::State<'_, NodeState>,
) -> Result<DiagNetworkInfo, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;
    Ok(DiagNetworkInfo {
        own_user_id: rt.user_id().to_string(),
        own_name: rt.display_name().to_string(),
        account_id: rt.account_id().to_string(),
        listen_tcp_port: rt.listen_tcp_port(),
        discovery_port: mesh_talk_core::node::DEFAULT_DISCOVERY_PORT,
        multicast_group: mesh_talk_core::node::DISCOVERY_MULTICAST_GROUP.to_string(),
        interfaces: mesh_talk_core::node::ipv4_interface_addrs()
            .into_iter()
            .map(|ip| ip.to_string())
            .collect(),
    })
}

// --- Presence (online / last-seen) ------------------------------------------

/// A peer is considered "online" if heard from within this window. Generous enough
/// to ride out a missed announce tick, tight enough that a departed peer dims promptly.
const PRESENCE_TTL_SECS: u64 = 30;

/// Per-conversation presence, keyed by account id (DMs) and channel id (channels).
#[derive(Serialize)]
pub struct PresenceInfo {
    /// True when at least one relevant device was heard from within the TTL.
    pub online: bool,
    /// Whole seconds since the most-recently-seen relevant device (None if never seen).
    pub last_seen_secs: Option<u64>,
}

/// Fold per-device "seconds since last seen" values into a [`PresenceInfo`]: the freshest
/// (minimum) device wins, and the conversation is online if that freshest sighting is
/// strictly within [`PRESENCE_TTL_SECS`]. An empty iterator yields offline / never-seen.
fn presence_from_seen(secs: impl Iterator<Item = u64>) -> PresenceInfo {
    let best = secs.min();
    PresenceInfo {
        online: best.is_some_and(|s| s < PRESENCE_TTL_SECS),
        last_seen_secs: best,
    }
}

/// A snapshot of presence for every account + channel conversation, keyed by id.
///
/// Online = the account (DM) has ≥1 device currently in the roster seen within the TTL;
/// for a channel, ≥1 known member device is present within the TTL. Reuses the same
/// roster the diagnostics commands read, so it's a cheap, lock-once snapshot. Polled by
/// the frontend on a slow interval into an isolated store (presence ticks must not
/// re-render the message list).
#[tauri::command]
pub async fn get_presence(
    state: tauri::State<'_, NodeState>,
) -> Result<std::collections::HashMap<String, PresenceInfo>, CommandError> {
    use std::collections::HashMap;
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(CommandError::not_started)?;

    // last_seen (whole secs) per user_id, from one roster snapshot.
    let peers = rt.peers();
    let seen_by_user: HashMap<String, u64> = peers
        .iter()
        .map(|p| (p.public.user_id(), p.last_seen.elapsed().as_secs()))
        .collect();

    let mut out: HashMap<String, PresenceInfo> = HashMap::new();

    // Per-account presence: the freshest of the account's known devices.
    let mut by_account: HashMap<String, Vec<u64>> = HashMap::new();
    for p in &peers {
        if let Some(acct) = &p.account_id {
            by_account
                .entry(acct.clone())
                .or_default()
                .push(p.last_seen.elapsed().as_secs());
        }
    }
    for (acct, secs) in by_account {
        out.insert(acct, presence_from_seen(secs.into_iter()));
    }

    // Per-channel presence: the freshest of any known member device currently in roster.
    for c in rt.list_channels() {
        let secs: Vec<u64> = rt
            .channel_members(c.id)
            .into_iter()
            .filter_map(|m| seen_by_user.get(&m.user_id()).copied())
            .collect();
        out.insert(
            hex::encode(c.id.as_bytes()),
            presence_from_seen(secs.into_iter()),
        );
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_channel_id_accepts_64_hex_chars() {
        let id = parse_channel_id(&"ab".repeat(32)).expect("valid id");
        assert_eq!(id.as_bytes(), &[0xab; 32]);
    }

    #[test]
    fn parse_channel_id_rejects_wrong_length() {
        assert!(parse_channel_id(&"ab".repeat(16)).is_err()); // 16 bytes, not 32
        assert!(parse_channel_id("").is_err());
    }

    #[test]
    fn parse_channel_id_rejects_non_hex() {
        assert!(parse_channel_id(&"zz".repeat(32)).is_err()); // not hex
        assert!(parse_channel_id("abc").is_err()); // odd-length hex
    }

    #[test]
    fn parse_event_id_accepts_valid_and_rejects_bad_input() {
        assert_eq!(
            parse_event_id(&"cd".repeat(32)).expect("valid").as_bytes(),
            &[0xcd; 32]
        );
        assert!(parse_event_id(&"cd".repeat(10)).is_err()); // too short
        assert!(parse_event_id(&"gg".repeat(32)).is_err()); // not hex
    }

    #[test]
    fn presence_fresh_device_is_online() {
        let p = presence_from_seen([5u64].into_iter());
        assert!(p.online);
        assert_eq!(p.last_seen_secs, Some(5));
    }

    #[test]
    fn presence_all_stale_is_offline_with_freshest_last_seen() {
        let p = presence_from_seen([60u64, 45, 90].into_iter());
        assert!(!p.online);
        assert_eq!(p.last_seen_secs, Some(45)); // freshest (min), still > TTL
    }

    #[test]
    fn presence_empty_is_offline_never_seen() {
        let p = presence_from_seen(std::iter::empty());
        assert!(!p.online);
        assert_eq!(p.last_seen_secs, None);
    }

    #[test]
    fn presence_uses_freshest_of_many_devices() {
        // One fresh device among stale ones makes the conversation online,
        // and last_seen reflects the freshest (min).
        let p = presence_from_seen([100u64, 3, 50].into_iter());
        assert!(p.online);
        assert_eq!(p.last_seen_secs, Some(3));
    }

    #[test]
    fn presence_ttl_boundary_is_strict() {
        // Exactly TTL is offline (strict `<`); one second under is online.
        let at = presence_from_seen([PRESENCE_TTL_SECS].into_iter());
        assert!(!at.online);
        assert_eq!(at.last_seen_secs, Some(PRESENCE_TTL_SECS));

        let under = presence_from_seen([PRESENCE_TTL_SECS - 1].into_iter());
        assert!(under.online);
        assert_eq!(under.last_seen_secs, Some(PRESENCE_TTL_SECS - 1));
    }
}
