//! Tauri events emitted to the frontend by the node. (Legacy
//! message/contact/network/file-transfer events were retired with the legacy stack.)

use crate::settings::SettingsState;
use mesh_talk_core::eventlog::event::EventId;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, Runtime};
use tauri_plugin_notification::NotificationExt;

/// How many characters of a message body we surface in a notification preview.
const PREVIEW_LEN: usize = 80;

#[derive(Clone, Default)]
pub struct NotificationContextState(Arc<Mutex<NotificationContext>>);

#[derive(Clone, Default)]
struct NotificationContext {
    active_peer_devices: Vec<String>,
    active_channel: Option<String>,
    revision: u64,
}

enum NotificationTarget<'a> {
    Dm(&'a str),
    Channel(&'a str),
    File,
}

#[tauri::command]
pub fn set_notification_context(
    state: tauri::State<'_, NotificationContextState>,
    active_peer_devices: Vec<String>,
    active_channel: Option<String>,
    revision: u64,
) -> Result<(), crate::commands::CommandError> {
    if active_peer_devices.len() > 32
        || active_peer_devices.iter().any(|id| id.len() > 64)
        || active_channel.as_ref().is_some_and(|id| id.len() > 64)
    {
        return Err(crate::commands::CommandError::Validation(
            "invalid notification context".into(),
        ));
    }
    let mut context = state.0.lock().map_err(|_| {
        crate::commands::CommandError::Internal("notification context unavailable".into())
    })?;
    if revision > context.revision {
        *context = NotificationContext {
            active_peer_devices,
            active_channel,
            revision,
        };
    }
    Ok(())
}

fn notification_payload(
    settings: &crate::settings::AppSettings,
    focused: bool,
    context: &NotificationContext,
    target: NotificationTarget<'_>,
    title: &str,
    body: &str,
) -> Option<(String, String)> {
    if !settings.notifications {
        return None;
    }
    if focused {
        let active_known =
            !context.active_peer_devices.is_empty() || context.active_channel.is_some();
        let already_visible = match target {
            NotificationTarget::Dm(device) => {
                context.active_peer_devices.iter().any(|id| id == device)
            }
            NotificationTarget::Channel(channel) => {
                context.active_channel.as_deref() == Some(channel)
            }
            NotificationTarget::File => true,
        };
        if already_visible || !active_known {
            return None;
        }
    }
    if !settings.notification_preview {
        let generic = match target {
            NotificationTarget::File => "New file",
            _ => "New message",
        };
        return Some(("Mesh-Talk".into(), generic.into()));
    }
    Some((title.into(), preview(body)))
}

/// Fire a native notification for an incoming item, but only when it would
/// actually be useful: notifications are enabled in settings AND the main window
/// is not currently focused (no point pinging the user about a message they can
/// already see). Best-effort — failures are logged, never propagated.
///
/// `body` must contain only what the UI already shows (decoded plaintext / a
/// generic "sent a file"); never raw ciphertext or anything more than the chat view.
fn notify_if_unfocused<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    target: NotificationTarget<'_>,
    title: &str,
    body: &str,
) {
    let mut settings = app_handle
        .try_state::<SettingsState>()
        .map(|s| s.get())
        .unwrap_or_default();
    if crate::app_lock::app_locked(app_handle) {
        settings.notification_preview = false;
    }
    let context = app_handle
        .try_state::<NotificationContextState>()
        .and_then(|s| s.0.lock().ok().map(|context| context.clone()))
        .unwrap_or_default();
    let focused = !crate::app_lock::app_locked(app_handle)
        && app_handle
            .get_webview_window("main")
            .is_some_and(|window| window.is_focused().unwrap_or(false));
    let Some((title, body)) =
        notification_payload(&settings, focused, &context, target, title, body)
    else {
        return;
    };

    if let Err(e) = app_handle
        .notification()
        .builder()
        .title(&title)
        .body(&body)
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
/// Progress of local file preparation or saving (throttled).
pub const EVENT_FILE_PROGRESS: &str = "file-progress";
/// A peer propagated their avatar (a signed profile) to us.
pub const EVENT_PROFILE_RECEIVED: &str = "profile-received";
/// An inbound call signal (WebRTC SDP / "bye") from an authenticated peer. Ephemeral.
pub const EVENT_CALL_SIGNAL: &str = "call-signal";

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
    if crate::app_lock::app_locked(app_handle) {
        return;
    }
    if let Err(e) = app_handle.emit(EVENT_PROFILE_RECEIVED, event) {
        log::error!("Failed to emit profile event: {e}");
    }
}

#[derive(serde::Serialize, Clone)]
pub struct CallSignalEvent {
    /// The AUTHENTICATED sender device user_id (bound to the Noise channel, not self-asserted).
    pub from: String,
    /// The opaque signaling JSON the peer's frontend produced (call id, kind, SDP).
    pub payload: String,
}

/// Emit an inbound call signal to the frontend. The payload is the peer's signaling JSON,
/// passed through verbatim (JSON is valid UTF-8). Ephemeral — nothing is persisted.
pub fn emit_call_signal<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    from: String,
    payload: Vec<u8>,
) {
    let event = CallSignalEvent {
        from,
        payload: String::from_utf8_lossy(&payload).into_owned(),
    };
    if crate::app_lock::app_locked(app_handle) {
        return;
    }
    if let Err(e) = app_handle.emit(EVENT_CALL_SIGNAL, event) {
        log::error!("Failed to emit call-signal event: {e}");
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
        if crate::app_lock::app_locked(&self.app_handle) {
            return;
        }
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
    pub is_channel: bool,
    pub from: String, // sender user-id
    pub name: String,
    pub size: u64,
    pub mime: String,      // so the UI can decide image/video/other
    pub file_conv: String, // hex — pass to save
    pub media: bool,       // inline media (media button) vs attachment (attach button), by intent
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
    notify_if_unfocused(
        app_handle,
        NotificationTarget::Dm(&event.from),
        &event.from_name,
        &event.text,
    );
    if crate::app_lock::app_locked(app_handle) {
        return;
    }
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
        NotificationTarget::Channel(&event.channel_id),
        &format!("# {}", event.channel_name),
        &event.text,
    );
    if crate::app_lock::app_locked(app_handle) {
        return;
    }
    if let Err(e) = app_handle.emit(EVENT_CHANNEL_MESSAGE, event) {
        log::error!("Failed to emit channel event: {e}");
    }
}

pub fn emit_file_received<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    conv: String,
    is_channel: bool,
    from: String,
    name: String,
    size: u64,
    mime: String,
    file_conv: String,
    media: bool,
) {
    let event = FileReceivedEvent {
        conv,
        is_channel,
        from,
        name,
        size,
        mime,
        file_conv,
        media,
    };
    notify_if_unfocused(
        app_handle,
        NotificationTarget::File,
        &event.from,
        "sent a file",
    );
    if crate::app_lock::app_locked(app_handle) {
        return;
    }
    if let Err(e) = app_handle.emit(EVENT_FILE_RECEIVED, event) {
        log::error!("Failed to emit file event: {e}");
    }
}

#[cfg(test)]
mod notification_policy_tests {
    use super::*;

    #[test]
    fn private_mode_never_places_sender_or_text_in_notification() {
        let settings = crate::settings::AppSettings::default()
            .with_privacy_level(crate::settings::PrivacyLevel::Private);
        let payload = notification_payload(
            &settings,
            false,
            &NotificationContext::default(),
            NotificationTarget::Dm("peer-device"),
            "Alice",
            "Secret text",
        )
        .unwrap();
        assert_eq!(payload, ("Mesh-Talk".into(), "New message".into()));
    }

    #[test]
    fn focused_chat_suppresses_only_its_own_messages() {
        let settings = crate::settings::AppSettings::default();
        let context = NotificationContext {
            active_peer_devices: vec!["active-device".into()],
            active_channel: Some("active-channel".into()),
            revision: 1,
        };
        assert!(
            notification_payload(
                &settings,
                true,
                &context,
                NotificationTarget::Dm("active-device"),
                "Alice",
                "Hello",
            )
            .is_none()
        );
        assert!(
            notification_payload(
                &settings,
                true,
                &context,
                NotificationTarget::Channel("other-channel"),
                "Other",
                "Hello",
            )
            .is_some()
        );
    }

    #[test]
    fn notifications_toggle_suppresses_every_target() {
        let settings = crate::settings::AppSettings {
            notifications: false,
            ..crate::settings::AppSettings::default()
        };
        assert!(
            notification_payload(
                &settings,
                false,
                &NotificationContext::default(),
                NotificationTarget::File,
                "Alice",
                "sent a file",
            )
            .is_none()
        );
    }
}
