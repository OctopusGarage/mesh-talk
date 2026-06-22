//! Tauri events emitted to the frontend by the node. (Legacy
//! message/contact/network/file-transfer events were retired with the legacy stack.)

use crate::settings::SettingsState;
use mesh_talk_core::eventlog::event::EventId;
use tauri::{Emitter, Manager, Runtime};
use tauri_plugin_notification::NotificationExt;

/// How many characters of a message body we surface in a notification preview.
const PREVIEW_LEN: usize = 80;

/// Fire a native notification for an incoming item, but only when it would
/// actually be useful: notifications are enabled in settings AND the main window
/// is not currently focused (no point pinging the user about a message they can
/// already see). Best-effort — failures are logged, never propagated.
///
/// `body` must contain only what the UI already shows (decoded plaintext / a
/// generic "sent a file"); never raw ciphertext or anything more than the chat view.
fn notify_if_unfocused<R: Runtime>(app_handle: &tauri::AppHandle<R>, title: &str, body: &str) {
    // Respect the user's toggle.
    let enabled = app_handle
        .try_state::<SettingsState>()
        .map(|s| s.get().notifications)
        .unwrap_or(true);
    if !enabled {
        return;
    }

    // Skip when the window is focused — the message is already on screen.
    if let Some(window) = app_handle.get_webview_window("main") {
        if window.is_focused().unwrap_or(false) {
            return;
        }
    }

    if let Err(e) = app_handle
        .notification()
        .builder()
        .title(title)
        .body(body)
        .show()
    {
        log::warn!("Failed to show notification: {e}");
    }
}

/// Truncate a preview to a sane length on a char boundary, appending an ellipsis.
fn preview(text: &str) -> String {
    if text.chars().count() <= PREVIEW_LEN {
        return text.to_string();
    }
    let truncated: String = text.chars().take(PREVIEW_LEN).collect();
    format!("{truncated}…")
}

/// A received DM, surfaced to the `/` UI.
pub const EVENT_DM_RECEIVED: &str = "dm-received";
/// A received channel message.
pub const EVENT_CHANNEL_MESSAGE: &str = "channel-message";
/// A received file (DM or channel).
pub const EVENT_FILE_RECEIVED: &str = "file-received";
/// Progress of an in-flight file send or save (throttled).
pub const EVENT_FILE_PROGRESS: &str = "file-progress";
/// A peer propagated their avatar (a signed profile) to us.
pub const EVENT_PROFILE_RECEIVED: &str = "profile-received";

#[derive(serde::Serialize, Clone)]
pub struct ProfileReceivedEvent {
    /// The sender's account id (the key the UI renders avatars by).
    pub account_id: String,
    /// The avatar data-URL, or `None` when the peer cleared their avatar.
    pub avatar: Option<String>,
}

/// Emit a received peer profile (avatar update) to the frontend. The avatar bytes are the
/// data-URL string the sender's UI produced; we pass them through verbatim (lossily UTF-8
/// decoded — a data-URL is ASCII, so this is lossless in practice).
pub fn emit_profile_received<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    account_id: String,
    avatar: Option<Vec<u8>>,
) {
    let event = ProfileReceivedEvent {
        account_id,
        avatar: avatar.map(|a| String::from_utf8_lossy(&a).into_owned()),
    };
    if let Err(e) = app_handle.emit(EVENT_PROFILE_RECEIVED, event) {
        log::error!("Failed to emit profile event: {e}");
    }
}

#[derive(serde::Serialize, Clone)]
pub struct DmReceivedEvent {
    pub from: String,
    pub from_name: String,
    pub text: String,
    pub reply_to: Option<String>, // hex EventId of the parent message, if any
}

#[derive(serde::Serialize, Clone)]
pub struct ChannelMessageEvent {
    pub channel_id: String, // hex
    pub channel_name: String,
    pub from: String,
    pub text: String,
    pub reply_to: Option<String>, // hex EventId of the parent message, if any
}

#[derive(serde::Serialize, Clone)]
pub struct FileProgressEvent {
    pub file_conv: String,       // hex — the per-file conversation handle
    pub direction: &'static str, // "send" | "save"
    pub done: u32,
    pub total: u32,
}

/// A throttled emitter for file-transfer progress: coalesces to ~10 emits/s but
/// ALWAYS emits the terminal `done == total`. Construct one per transfer; call
/// [`ProgressThrottle::emit`] on each chunk callback.
pub struct ProgressThrottle<R: Runtime> {
    app_handle: tauri::AppHandle<R>,
    file_conv: String,
    direction: &'static str,
    last: std::time::Instant,
}

impl<R: Runtime> ProgressThrottle<R> {
    /// ~10 emits/s.
    const MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

    pub fn new(
        app_handle: tauri::AppHandle<R>,
        file_conv: String,
        direction: &'static str,
    ) -> Self {
        // Back-date so the first progress tick always emits.
        Self {
            app_handle,
            file_conv,
            direction,
            last: std::time::Instant::now() - Self::MIN_INTERVAL,
        }
    }

    pub fn emit(&mut self, done: u32, total: u32) {
        let terminal = done >= total;
        if !terminal && self.last.elapsed() < Self::MIN_INTERVAL {
            return;
        }
        self.last = std::time::Instant::now();
        let event = FileProgressEvent {
            file_conv: self.file_conv.clone(),
            direction: self.direction,
            done,
            total,
        };
        if let Err(e) = self.app_handle.emit(EVENT_FILE_PROGRESS, event) {
            log::error!("Failed to emit file-progress event: {e}");
        }
    }
}

#[derive(serde::Serialize, Clone)]
pub struct FileReceivedEvent {
    pub conv: String, // hex (channel id for a channel file; the DM conv otherwise)
    pub from: String, // sender user-id
    pub name: String,
    pub size: u64,
    pub mime: String,      // so the UI can decide image/video/other
    pub file_conv: String, // hex — pass to save
}

/// Emit a received DM to the frontend (text decoded lossily for display).
pub fn emit_dm_received<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    from: String,
    from_name: String,
    text: Vec<u8>,
    reply_to: Option<EventId>,
) {
    let event = DmReceivedEvent {
        from,
        from_name,
        text: String::from_utf8_lossy(&text).into_owned(),
        reply_to: reply_to.map(|id| hex::encode(id.as_bytes())),
    };
    notify_if_unfocused(app_handle, &event.from_name, &preview(&event.text));
    if let Err(e) = app_handle.emit(EVENT_DM_RECEIVED, event) {
        log::error!("Failed to emit dm event: {e}");
    }
}

pub fn emit_channel_message<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    channel_id: String,
    channel_name: String,
    from: String,
    text: Vec<u8>,
    reply_to: Option<EventId>,
) {
    let event = ChannelMessageEvent {
        channel_id,
        channel_name,
        from,
        text: String::from_utf8_lossy(&text).into_owned(),
        reply_to: reply_to.map(|id| hex::encode(id.as_bytes())),
    };
    notify_if_unfocused(
        app_handle,
        &format!("# {}", event.channel_name),
        &preview(&event.text),
    );
    if let Err(e) = app_handle.emit(EVENT_CHANNEL_MESSAGE, event) {
        log::error!("Failed to emit channel event: {e}");
    }
}

pub fn emit_file_received<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    conv: String,
    from: String,
    name: String,
    size: u64,
    mime: String,
    file_conv: String,
) {
    let event = FileReceivedEvent {
        conv,
        from,
        name,
        size,
        mime,
        file_conv,
    };
    notify_if_unfocused(app_handle, &event.from, "sent a file");
    if let Err(e) = app_handle.emit(EVENT_FILE_RECEIVED, event) {
        log::error!("Failed to emit file event: {e}");
    }
}
