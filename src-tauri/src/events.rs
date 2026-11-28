//! Tauri events emitted to the frontend by the node. (Legacy
//! message/contact/network/file-transfer events were retired with the legacy stack.)

use crate::settings::SettingsState;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, Runtime};

/// How many characters of a message body we surface in a notification preview.
const PREVIEW_LEN: usize = 80;

mod presentation;
pub(crate) use presentation::prepare_notification;

/// Native presentation starts on the main thread before acquiring session
/// authority. The completion receiver lets bounded producers keep one queued
/// batch until actual execution, including rejection of a retired authority.
pub(crate) fn dispatch_runtime_presentation<R: Runtime>(
    app: &tauri::AppHandle<R>,
    authority: &crate::chat_commands::RuntimeAuthority,
    present: impl FnOnce(&tauri::AppHandle<R>) + Send + 'static,
) -> Result<PresentationCompletion, tauri::Error> {
    let queued_app = app.clone();
    queue_authorized_presentation(
        authority.queued_current_with_session(move |info| {
            if let Some(state) = queued_app.try_state::<NotificationContextState>()
                && let Ok(mut name) = state.2.lock()
            {
                *name = if info.user.display_name.is_empty() {
                    info.user.login_name().to_owned()
                } else {
                    info.user.display_name.clone()
                };
            }
            present(&queued_app);
        }),
        |task| app.run_on_main_thread(task),
    )
}

type PendingPresentation = Arc<std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>>;

pub(crate) struct PresentationCompletion {
    completion: tokio::sync::oneshot::Receiver<()>,
    pending: PendingPresentation,
}
impl PresentationCompletion {
    pub(crate) async fn wait(mut self) -> Result<(), tokio::sync::oneshot::error::RecvError> {
        (&mut self.completion).await
    }
}
impl Drop for PresentationCompletion {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.take();
        }
    }
}

#[cfg(test)]
fn queue_runtime_presentation<E>(
    authority: crate::chat_commands::RuntimeAuthority,
    present: impl FnOnce() + Send + 'static,
    enqueue: impl FnOnce(Box<dyn FnOnce() + Send>) -> Result<(), E>,
) -> Result<PresentationCompletion, E> {
    queue_authorized_presentation(authority.queued_current(present), enqueue)
}

fn queue_authorized_presentation<E>(
    present: impl FnOnce() + Send + 'static,
    enqueue: impl FnOnce(Box<dyn FnOnce() + Send>) -> Result<(), E>,
) -> Result<PresentationCompletion, E> {
    let (finished, completion) = tokio::sync::oneshot::channel();
    let pending: PendingPresentation = Arc::new(std::sync::Mutex::new(Some(Box::new(present))));
    let queued = Arc::downgrade(&pending);
    enqueue(Box::new(move || {
        // The runtime task owns the payload until actual execution. Cancelling
        // that task frees it immediately, even while the main thread is busy.
        let present = queued
            .upgrade()
            .and_then(|pending| pending.lock().ok()?.take());
        if let Some(present) = present {
            present();
        }
        let _ = finished.send(());
    }))?;
    Ok(PresentationCompletion {
        completion,
        pending,
    })
}

#[derive(Clone, Default)]
pub struct NotificationContextState(
    Arc<Mutex<NotificationContext>>,
    Arc<Mutex<crate::channel_reception::ReceptionLedger>>,
    Arc<Mutex<String>>,
);

#[derive(Clone, Default)]
struct NotificationContext {
    active_peer_devices: Vec<String>,
    active_channel: Option<String>,
    revision: u64,
    language: Option<String>,
}

enum NotificationTarget<'a> {
    Dm(&'a str),
    Channel(&'a str),
    File,
    MentionedChannel(&'a str),
}

#[tauri::command]
pub fn set_notification_context(
    state: tauri::State<'_, NotificationContextState>,
    active_peer_devices: Vec<String>,
    active_channel: Option<String>,
    revision: u64,
    language: Option<String>,
) -> Result<(), crate::commands::CommandError> {
    if language.as_ref().is_some_and(|value| value.len() > 16)
        || active_peer_devices.len() > 32
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
            language,
        };
    }
    Ok(())
}

/// Match the frontend whole-message code-card contract before mention classification.
fn is_code_message(text: &str) -> bool {
    let Some((opening, rest)) = text.split_once('\n') else {
        return false;
    };
    let fence_len = opening.bytes().take_while(|byte| *byte == b'`').count();
    if fence_len < 3 {
        return false;
    }
    let language = &opening[fence_len..];
    if language.len() > 32
        || !language
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_+-".contains(&byte))
    {
        return false;
    }
    let fence = &opening[..fence_len];
    let ending = format!("\n{fence}");
    rest.strip_suffix(&ending)
        .is_some_and(|code| !code.split('\n').any(|line| line == fence))
}

/// Preserve the existing display-name mention convention; this is not identity metadata.
fn mentions_display_name(text: &str, name: &str) -> bool {
    if name.is_empty() || is_code_message(text) {
        return false;
    }
    // Bound the cache to one display name; never retain message content.
    static MATCHER: std::sync::OnceLock<Mutex<Option<(String, regex::Regex)>>> =
        std::sync::OnceLock::new();
    let Ok(mut cache) = MATCHER.get_or_init(|| Mutex::new(None)).lock() else {
        return false;
    };
    if cache.as_ref().is_none_or(|(cached, _)| cached != name) {
        let pattern = format!(
            r"(?i)(?:^|[^\p{{L}}\p{{N}}_@])@{}(?:$|[^\p{{L}}\p{{N}}_-])",
            regex::escape(name)
        );
        let Ok(matcher) = regex::Regex::new(&pattern) else {
            return false;
        };
        *cache = Some((name.to_owned(), matcher));
    }
    cache
        .as_ref()
        .is_some_and(|(_, matcher)| matcher.is_match(text))
}

fn notification_payload(
    settings: &crate::settings::AppSettings,
    focused: bool,
    context: &NotificationContext,
    target: NotificationTarget<'_>,
    title: &str,
    body: &str,
) -> Option<(String, String)> {
    if let NotificationTarget::Channel(channel) = target
        && settings.muted_groups.iter().any(|id| id == channel)
    {
        return None;
    }
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
            NotificationTarget::Channel(channel)
            | NotificationTarget::MentionedChannel(channel) => {
                context.active_channel.as_deref() == Some(channel)
            }
            NotificationTarget::File => true,
        };
        if already_visible || !active_known {
            return None;
        }
    }
    if !settings.notification_preview {
        let labels = presentation::words(context.language.as_deref().unwrap_or("en"));
        let generic = if context.language.is_none() && matches!(target, NotificationTarget::File) {
            "New file"
        } else {
            labels.message
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
/// typed attachment summary); never raw ciphertext or anything more than the chat view.
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

    if let Err(e) = crate::notifications::send(app_handle, &title, &body) {
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
/// Group metadata changed; the UI should refresh its authorized projection.
pub const EVENT_CHANNEL_CHANGED: &str = "channel-changed";
/// A received file (DM or channel).
pub const EVENT_FILE_RECEIVED: &str = "file-received";
/// Progress of local file preparation or saving (throttled).
pub const EVENT_FILE_PROGRESS: &str = "file-progress";
/// A peer propagated their avatar (a signed profile) to us.
pub const EVENT_PROFILE_RECEIVED: &str = "profile-received";
/// An inbound call signal (WebRTC SDP / "bye") from an authenticated peer. Ephemeral.
pub const EVENT_CALL_SIGNAL: &str = "call-signal";

#[derive(serde::Serialize, Clone)]
pub(crate) struct ChannelChangedEvent {
    /// Host login namespace; independent of the cryptographic account.
    pub owner: String,
    pub account_id: String,
    /// Device user ID, including after adopting a linked account.
    pub device_id: String,
    /// None requests a full group refresh after the bounded receiver lagged.
    pub channel_id: Option<String>,
}

pub(crate) fn emit_channel_changed<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    event: ChannelChangedEvent,
) {
    if crate::reception_host::invalidate_identity(
        app_handle,
        &event.owner,
        &event.account_id,
        &event.device_id,
    ) {
        return;
    }
    if crate::app_lock::app_locked(app_handle) {
        return;
    }
    if let Err(error) = app_handle.emit(EVENT_CHANNEL_CHANGED, event) {
        log::warn!("Failed to emit channel metadata change: {error}");
    }
}

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received_via: Option<mesh_talk_core::node::ReceivedVia>,
    pub protection: Option<mesh_talk_core::node::MessageProtection>,
    pub message_id: Option<String>,
    pub from_account: Option<String>,
    pub from: String,
    pub from_name: String,
    pub text: String,
    pub reply_to: Option<String>, // hex EventId of the parent message, if any
}

#[derive(serde::Serialize, Clone)]
pub struct ChannelMessageEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received_via: Option<mesh_talk_core::node::ReceivedVia>,
    pub protection: Option<mesh_talk_core::node::MessageProtection>,
    pub channel_id: String, // hex
    pub channel_name: String,
    pub from: String,
    pub text: String,
    pub reply_to: Option<String>, // hex EventId of the parent message, if any
    pub event_id: String,
    pub authored_at: u64,
    /// First successful local decryption observation, not a sender clock.
    pub received_at: u64,
    pub arrival_kind: &'static str,
    pub own_device: bool,
    pub batch_id: Option<String>,
    pub batch_complete: bool,
    pub batch_size: usize,
    pub reception_only: bool,
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
    reception_scope: Option<crate::reception_host::Scope>,
    legacy_delivery: bool,
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
        let reception_scope = app_handle
            .try_state::<crate::reception_host::ReceptionHostState>()
            .and_then(|state| state.scope());
        let legacy_delivery = app_handle
            .try_state::<crate::reception_host::ReceptionHostState>()
            .is_none();
        Self {
            app_handle,
            file_conv,
            direction,
            reception_scope,
            legacy_delivery,
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
        if let Some(scope) = &self.reception_scope {
            crate::reception_host::progress(&self.app_handle, scope, event);
            return;
        }
        if !self.legacy_delivery {
            return;
        }
        if let Err(e) = self.app_handle.emit(EVENT_FILE_PROGRESS, event) {
            log::error!("Failed to emit file-progress event: {e}");
        }
    }
}

#[derive(serde::Serialize, Clone)]
pub struct FileReceivedEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received_via: Option<mesh_talk_core::node::ReceivedVia>,
    pub protection: Option<mesh_talk_core::node::MessageProtection>,
    pub expires_at_ms: Option<u64>,
    pub conv: String, // hex (channel id for a channel file; the DM conv otherwise)
    pub is_channel: bool,
    pub from: String, // sender user-id
    pub name: String,
    pub size: u64,
    pub mime: String,      // so the UI can decide image/video/other
    pub file_conv: String, // hex — pass to save
    pub media: bool,       // inline media (media button) vs attachment (attach button), by intent
    pub event_id: String,
    pub authored_at: u64,
    pub received_at: u64,
    pub arrival_kind: &'static str,
    pub own_device: bool,
    pub batch_id: Option<String>,
    pub batch_complete: bool,
    pub batch_size: usize,
}

pub(crate) fn dm_event(dm: mesh_talk_core::node::ReceivedDm) -> DmReceivedEvent {
    DmReceivedEvent {
        received_via: dm.received_via,
        protection: dm.protection,
        message_id: dm.message_id.map(|id| id.to_hex()),
        from_account: dm.from_account,
        from: dm.from,
        from_name: dm.from_name,
        text: if dm.protection.is_some() {
            String::new()
        } else {
            String::from_utf8_lossy(&dm.text).into_owned()
        },
        reply_to: dm.reply_to.map(|id| hex::encode(id.as_bytes())),
    }
}

pub(crate) fn channel_event(
    msg: mesh_talk_core::node::ReceivedChannelMessage,
) -> ChannelMessageEvent {
    ChannelMessageEvent {
        received_via: msg.received_via,
        protection: msg.protection,
        channel_id: hex::encode(msg.channel_id.as_bytes()),
        channel_name: msg.channel_name,
        from: msg.from,
        text: if msg.protection.is_some() {
            String::new()
        } else {
            String::from_utf8_lossy(&msg.text).into_owned()
        },
        reply_to: msg.reply_to.map(|id| hex::encode(id.as_bytes())),
        event_id: msg.event_id.to_hex(),
        authored_at: msg.wall_clock,
        received_at: msg.received_at,
        arrival_kind: msg.arrival_kind,
        own_device: msg.own_device,
        batch_id: msg.batch_id,
        batch_complete: msg.batch_complete,
        batch_size: msg.batch_size,
        reception_only: msg.reception_only,
    }
}

pub(crate) fn file_event(file: mesh_talk_core::node::ReceivedFile) -> FileReceivedEvent {
    FileReceivedEvent {
        received_via: file.received_via,
        protection: file.protection,
        expires_at_ms: file.expires_at_ms,
        conv: hex::encode(file.conv.as_bytes()),
        is_channel: file.is_channel,
        from: file.from,
        name: if file.protection.is_some() {
            "Protected image".into()
        } else {
            file.name
        },
        size: file.size,
        mime: file.mime,
        file_conv: hex::encode(file.file_conv.as_bytes()),
        media: file.media,
        event_id: file.event_id.to_hex(),
        authored_at: file.authored_at,
        received_at: file.received_at,
        arrival_kind: file.arrival_kind,
        own_device: file.own_device,
        batch_id: file.batch_id,
        batch_complete: file.batch_complete,
        batch_size: file.batch_size,
    }
}

/// Emit a received DM to the frontend (text decoded lossily for display).
pub fn emit_dm_received<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    dm: mesh_talk_core::node::ReceivedDm,
) {
    publish_dm_received(app_handle, dm, true, None);
}

fn publish_dm_received<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    dm: mesh_talk_core::node::ReceivedDm,
    publish: bool,
    presentation: Option<presentation::NotificationPresentation>,
) {
    let event = dm_event(dm);
    notify_if_unfocused(
        app_handle,
        NotificationTarget::Dm(&event.from),
        presentation
            .as_ref()
            .map_or_else(
                || presentation::display_name(&event.from_name, "Unknown contact"),
                |p| p.title.clone(),
            )
            .as_str(),
        if event.protection.is_some() {
            presentation
                .as_ref()
                .map_or("Protected message", |p| p.body.as_str())
        } else {
            presentation
                .as_ref()
                .map_or(event.text.as_str(), |p| p.body.as_str())
        },
    );
    if !publish || crate::app_lock::app_locked(app_handle) {
        return;
    }
    if let Err(e) = app_handle.emit(EVENT_DM_RECEIVED, event) {
        log::error!("Failed to emit dm event: {e}");
    }
}

pub fn emit_channel_message<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    owner: &str,
    msg: mesh_talk_core::node::ReceivedChannelMessage,
) {
    publish_channel_message(app_handle, owner, msg, true, None);
}

fn publish_channel_message<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    owner: &str,
    msg: mesh_talk_core::node::ReceivedChannelMessage,
    publish: bool,
    presentation: Option<presentation::NotificationPresentation>,
) {
    use crate::channel_reception::Alert;
    let event = channel_event(msg);
    let alert = app_handle
        .try_state::<NotificationContextState>()
        .and_then(|state| {
            let mut ledger = state.1.lock().ok()?;
            Some(if event.reception_only {
                ledger.complete(
                    owner,
                    &event.channel_id,
                    event.batch_id.as_deref().unwrap_or(&event.event_id),
                )
            } else {
                ledger.accept(
                    owner,
                    &event.channel_id,
                    &event.event_id,
                    event.arrival_kind,
                    event.own_device,
                    event.batch_id.as_deref(),
                    event.batch_complete,
                )
            })
        });
    let Some(decision) = alert else {
        return;
    };
    match decision.alert {
        Alert::Live => notify_if_unfocused(
            app_handle,
            if !event.own_device
                && event.protection.is_none()
                && app_handle
                    .try_state::<NotificationContextState>()
                    .and_then(|state| {
                        state
                            .2
                            .lock()
                            .ok()
                            .map(|name| mentions_display_name(&event.text, &name))
                    })
                    .unwrap_or(false)
            {
                NotificationTarget::MentionedChannel(&event.channel_id)
            } else {
                NotificationTarget::Channel(&event.channel_id)
            },
            presentation
                .as_ref()
                .map_or_else(
                    || presentation::display_name(&event.channel_name, "Group chat"),
                    |p| p.title.clone(),
                )
                .as_str(),
            if event.protection.is_some() {
                presentation
                    .as_ref()
                    .map_or("Protected message", |p| p.body.as_str())
            } else {
                presentation
                    .as_ref()
                    .map_or(event.text.as_str(), |p| p.body.as_str())
            },
        ),
        Alert::Summary(count) => notify_if_unfocused(
            app_handle,
            NotificationTarget::Channel(&event.channel_id),
            presentation
                .as_ref()
                .map_or_else(
                    || presentation::display_name(&event.channel_name, "Group chat"),
                    |p| p.title.clone(),
                )
                .as_str(),
            &presentation.as_ref().map_or_else(
                || format!("{count} messages received while catching up"),
                |p| p.summary(count),
            ),
        ),
        Alert::Silent => {}
    }
    if !publish || !decision.emit || crate::app_lock::app_locked(app_handle) {
        return;
    }
    if let Err(e) = app_handle.emit(EVENT_CHANNEL_MESSAGE, event) {
        log::error!("Failed to emit channel event: {e}");
    }
}

fn file_notification_body(
    name: &str,
    mime: &str,
    size: u64,
    media: bool,
    protected: bool,
) -> String {
    presentation::attachment(
        name,
        mime,
        size,
        media,
        protected,
        &presentation::words("en"),
    )
}

pub fn emit_file_received<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    owner: &str,
    file: mesh_talk_core::node::ReceivedFile,
) {
    publish_file_received(app_handle, owner, file, true, None);
}

fn publish_file_received<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    owner: &str,
    file: mesh_talk_core::node::ReceivedFile,
    publish: bool,
    presentation: Option<presentation::NotificationPresentation>,
) {
    use crate::channel_reception::Alert;
    let event = file_event(file);
    let decision = app_handle
        .try_state::<NotificationContextState>()
        .and_then(|state| {
            Some(state.1.lock().ok()?.accept(
                owner,
                &event.conv,
                &event.event_id,
                event.arrival_kind,
                event.own_device,
                event.batch_id.as_deref(),
                event.batch_complete,
            ))
        });
    let Some(decision) = decision else {
        return;
    };
    let target = if event.is_channel {
        NotificationTarget::Channel(&event.conv)
    } else if presentation.is_some() {
        NotificationTarget::Dm(&event.from)
    } else {
        NotificationTarget::File
    };
    match decision.alert {
        Alert::Live => notify_if_unfocused(
            app_handle,
            target,
            presentation
                .as_ref()
                .map_or("Unknown contact", |p| p.title.as_str()),
            &presentation
                .as_ref()
                .map(|p| p.body.clone())
                .unwrap_or_else(|| {
                    file_notification_body(
                        &event.name,
                        &event.mime,
                        event.size,
                        event.media,
                        event.protection.is_some(),
                    )
                }),
        ),
        Alert::Summary(count) => notify_if_unfocused(
            app_handle,
            target,
            presentation
                .as_ref()
                .map_or("Mesh-Talk", |p| p.title.as_str()),
            &presentation.as_ref().map_or_else(
                || format!("{count} file messages received while catching up"),
                |p| p.summary(count),
            ),
        ),
        Alert::Silent => {}
    }
    if !publish || !decision.emit || crate::app_lock::app_locked(app_handle) {
        return;
    }
    if let Err(e) = app_handle.emit(EVENT_FILE_RECEIVED, event) {
        log::error!("Failed to emit file event: {e}");
    }
}

#[cfg(test)]
mod notification_policy_tests {
    #[test]
    fn group_mute_preserves_only_explicit_mention_attention() {
        let mut settings = crate::settings::AppSettings {
            muted_groups: vec!["group".into()],
            ..Default::default()
        };
        let context = super::NotificationContext::default();
        assert!(
            super::notification_payload(
                &settings,
                false,
                &context,
                super::NotificationTarget::Channel("group"),
                "Group",
                "hello"
            )
            .is_none()
        );
        assert!(
            super::notification_payload(
                &settings,
                false,
                &context,
                super::NotificationTarget::MentionedChannel("group"),
                "Group",
                "@Alice"
            )
            .is_some()
        );
        settings.notifications = false;
        assert!(
            super::notification_payload(
                &settings,
                false,
                &context,
                super::NotificationTarget::MentionedChannel("group"),
                "Group",
                "@Alice"
            )
            .is_none()
        );
        assert!(super::mentions_display_name(
            "hi @Alice Smith!",
            "Alice Smith"
        ));
        assert!(super::mentions_display_name("@亏麻了 !!!", "亏麻了"));
        assert!(!super::mentions_display_name("mail@alice.com", "alice"));
        assert!(!super::mentions_display_name("@alice_2", "alice"));
        assert!(super::mentions_display_name("@ſ", "s"));
        assert!(!super::mentions_display_name(
            "```python\n@alice\n```",
            "alice"
        ));
        assert!(super::mentions_display_name(
            "text ```\n@alice\n```",
            "alice"
        ));
        assert!(!super::mentions_display_name("@alice²", "alice"));
    }

    #[test]
    fn queued_presentation_reuses_current_session_identity_without_relocking() {
        let (_, _, authority) = presentation_authority();
        let observed = Arc::new(Mutex::new(String::new()));
        let result = observed.clone();
        let mut queued = None;
        let _completion = queue_authorized_presentation(
            authority.queued_current_with_session(move |info| {
                *result.lock().unwrap() = info.user.display_name.clone();
            }),
            |task| {
                queued = Some(task);
                Ok::<_, ()>(())
            },
        )
        .unwrap();
        queued.unwrap()();
        assert_eq!(*observed.lock().unwrap(), "Fixture");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unavailable_native_provider_is_best_effort_at_the_unfocused_message_boundary() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        assert!(crate::notifications::send(app.handle(), "Fixture", "Message").is_err());
        assert!(
            super::notification_payload(
                &crate::settings::AppSettings::default(),
                false,
                &super::NotificationContext::default(),
                super::NotificationTarget::File,
                "Fixture",
                "Message",
            )
            .is_some()
        );
        super::notify_if_unfocused(
            app.handle(),
            super::NotificationTarget::File,
            "Fixture",
            "Message",
        );
    }

    use super::*;
    use tauri::Listener;

    fn presentation_authority() -> (
        crate::state::SessionState,
        crate::chat_commands::NodeState,
        crate::chat_commands::RuntimeAuthority,
    ) {
        let session = crate::state::SessionState::default();
        let node = crate::chat_commands::NodeState::empty();
        let lease = session.publish(
            "fixture token".into(),
            crate::services::user::User {
                user_id: "fixture-owner".into(),
                account_login_name: None,
                name: "fixture".into(),
                display_name: "Fixture".into(),
                address: "fixture".into(),
                created_at: 0,
                last_seen: 0,
                is_online: true,
            },
            "fixture password".into(),
        );
        let authority =
            crate::chat_commands::RuntimeAuthority::request(session.clone(), lease, node.clone())
                .unwrap();
        (session, node, authority)
    }

    #[test]
    fn notification_presentation_queues_without_waiting_for_the_main_thread_session_lock() {
        use std::{sync::mpsc, thread, time::Duration};
        let (session, _node, authority) = presentation_authority();
        let (queued_tx, queued_rx) = mpsc::channel();
        let (returned_tx, returned_rx) = mpsc::channel();
        let presented = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = presented.clone();
        let main_thread = thread::current().id();

        // Model a main-thread owner command holding the real Session mutex.
        // The runtime callback must enqueue and return while that guard is held,
        // rather than acquiring Session first or waiting on window focus.
        let (worker, queued, returned) = session
            .with_owner(|owner| {
                assert_eq!(owner, Some("fixture-owner"));
                let worker = thread::spawn(move || {
                    let completion = queue_runtime_presentation(
                        authority,
                        move || {
                            assert_eq!(thread::current().id(), main_thread);
                            observed.store(true, std::sync::atomic::Ordering::SeqCst);
                        },
                        |task| queued_tx.send(task),
                    )
                    .unwrap();
                    assert!(returned_tx.send(completion).is_ok());
                });
                let queued = queued_rx.recv_timeout(Duration::from_secs(2));
                let returned = returned_rx.recv_timeout(Duration::from_secs(2));
                Ok((worker, queued, returned))
            })
            .unwrap();
        // Release the simulated command before joining, including on regression.
        worker.join().unwrap();
        assert!(
            returned.is_ok(),
            "runtime callback must not wait for main-thread execution"
        );
        let _completion = returned.unwrap();
        assert!(!presented.load(std::sync::atomic::Ordering::SeqCst));
        queued.expect("presentation must queue without acquiring the Session mutex")();
        assert!(presented.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn notification_presentation_drops_queued_work_after_session_or_runtime_replacement() {
        for replacement in ["logout", "same_owner_session", "runtime"] {
            let (session, node, authority) = presentation_authority();
            let mut queued = None;
            let presented = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let observed = presented.clone();
            let _completion = queue_runtime_presentation(
                authority.clone(),
                move || {
                    observed.store(true, std::sync::atomic::Ordering::SeqCst);
                },
                |task| {
                    queued = Some(task);
                    Ok::<_, ()>(())
                },
            )
            .unwrap();
            match replacement {
                "logout" => session.clear(),
                "same_owner_session" => {
                    let info = session
                        .matching(&authority.lease, |info| info.clone())
                        .unwrap();
                    session.publish(info.token, info.user, info.password);
                }
                _ => {
                    crate::chat_commands::RuntimeAuthority::request(
                        session.clone(),
                        authority.lease.clone(),
                        node,
                    )
                    .unwrap();
                }
            }
            queued.unwrap()();
            assert!(
                !presented.load(std::sync::atomic::Ordering::SeqCst),
                "stale {replacement} must not notify or emit private events"
            );
        }
    }

    #[test]
    fn notification_presentation_completion_tracks_actual_execution_and_stale_rejection() {
        use tokio::sync::oneshot::error::TryRecvError;
        for retire in [false, true] {
            let (session, _node, authority) = presentation_authority();
            let presented = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let observed = presented.clone();
            let mut queued = None;
            let mut completion = queue_runtime_presentation(
                authority,
                move || observed.store(true, std::sync::atomic::Ordering::SeqCst),
                |task| {
                    queued = Some(task);
                    Ok::<_, ()>(())
                },
            )
            .unwrap();
            assert_eq!(completion.completion.try_recv(), Err(TryRecvError::Empty));
            if retire {
                session.clear();
            }
            queued.unwrap()();
            assert_eq!(completion.completion.try_recv(), Ok(()));
            assert_eq!(presented.load(std::sync::atomic::Ordering::SeqCst), !retire);
        }
    }

    #[test]
    fn notification_presentation_cancellation_releases_queued_payload_before_execution() {
        let (_, _, authority) = presentation_authority();
        let payload = Arc::new(vec![0u8; 8192]);
        let retained = Arc::downgrade(&payload);
        let mut queued = None;
        let completion = queue_runtime_presentation(
            authority,
            move || {
                drop(payload);
                panic!("cancelled presentation must not execute");
            },
            |task| {
                queued = Some(task);
                Ok::<_, ()>(())
            },
        )
        .unwrap();
        assert!(retained.upgrade().is_some());
        drop(completion);
        assert!(
            retained.upgrade().is_none(),
            "retired runtime releases payload while main thread remains blocked"
        );
        queued.unwrap()();
    }

    #[test]
    fn notification_presentation_propagates_enqueue_failure() {
        let (_, _, authority) = presentation_authority();
        let result = queue_runtime_presentation(
            authority,
            || panic!("failed enqueue must not execute presentation"),
            |_| Err("main thread unavailable"),
        );
        assert!(matches!(result, Err("main thread unavailable")));
    }

    #[test]
    fn sticker_notification_uses_validated_label_and_preserves_protection() {
        let v2 = "mt-sticker-v2-cGFjazpjYXRzOmNhbG0K8J-ZggrpnZnkuIvlv4M.png";
        let v1 = "mt-sticker-v1-YnVpbHRpbjp3YXZlCvCfkYs.png";
        assert_eq!(
            file_notification_body(v2, "image/png", 100, true, false),
            "[Sticker]"
        );
        assert_eq!(
            file_notification_body(v1, "image/png", 100, true, false),
            "[Sticker]"
        );
        assert_eq!(
            file_notification_body(v2, "image/png", 100, true, true),
            "[Protected message]"
        );
        assert_eq!(
            file_notification_body("private.png", "image/png", 100, true, false),
            "[Photo]"
        );
        assert_eq!(
            file_notification_body(v2, "image/png", 100, false, false),
            format!("[File] {v2}")
        );
    }

    #[test]
    fn dm_event_preserves_logical_identity_and_legacy_absence_for_unread_consumers() {
        use mesh_talk_core::{eventlog::EventId, node::ReceivedDm};
        let app = tauri::test::mock_app();
        let settings = SettingsState::default();
        settings.set(crate::settings::AppSettings {
            notifications: false,
            ..Default::default()
        });
        assert!(app.manage(settings));
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = received.clone();
        app.listen(EVENT_DM_RECEIVED, move |event| {
            sink.lock()
                .unwrap()
                .push(serde_json::from_str::<serde_json::Value>(event.payload()).unwrap());
        });
        let message_id = EventId::new([1; 32]);
        let reply_to = EventId::new([2; 32]);
        for (identity, account) in [
            (Some(message_id), Some("sender-account".to_string())),
            (None, None),
        ] {
            emit_dm_received(
                app.handle(),
                ReceivedDm {
                    received_via: None,
                    message_id: identity,
                    protection: None,
                    from_account: account,
                    from: "sender-device".into(),
                    from_name: "Sender".into(),
                    text: b"one received message".to_vec(),
                    reply_to: Some(reply_to),
                },
            );
        }
        let received = received.lock().unwrap();
        assert_eq!(received.len(), 2);
        for (index, event) in received.iter().enumerate() {
            assert_eq!(event["from"], "sender-device");
            assert_eq!(event["from_name"], "Sender");
            assert_eq!(event["text"], "one received message");
            assert_eq!(event["reply_to"], reply_to.to_hex());
            if index == 0 {
                assert_eq!(event["message_id"], message_id.to_hex());
                assert_eq!(event["from_account"], "sender-account");
            } else {
                assert!(event["message_id"].is_null());
                assert!(event["from_account"].is_null());
            }
        }
    }

    #[test]
    fn protected_image_event_preserves_protection_and_safe_file_metadata() {
        let app = tauri::test::mock_app();
        let settings = SettingsState::default();
        settings.set(crate::settings::AppSettings {
            notifications: false,
            ..Default::default()
        });
        app.manage(settings);
        app.manage(NotificationContextState::default());
        let received = Arc::new(Mutex::new(None));
        let sink = received.clone();
        app.listen(EVENT_FILE_RECEIVED, move |event| {
            *sink.lock().unwrap() =
                Some(serde_json::from_str::<serde_json::Value>(event.payload()).unwrap());
        });
        emit_file_received(
            app.handle(),
            "owner",
            mesh_talk_core::node::ReceivedFile {
                received_via: None,
                protection: Some(mesh_talk_core::node::MessageProtection::Authenticate),
                expires_at_ms: None,
                conv: mesh_talk_core::eventlog::ConversationId::new([1; 32]),
                is_channel: false,
                from: "sender".into(),
                name: "private.png".into(),
                size: 100,
                mime: "image/png".into(),
                file_conv: mesh_talk_core::eventlog::ConversationId::new([2; 32]),
                media: true,
                event_id: mesh_talk_core::eventlog::EventId::new([3; 32]),
                authored_at: 1,
                received_at: 2,
                arrival_kind: "live",
                own_device: false,
                batch_id: None,
                batch_complete: false,
                batch_size: 1,
            },
        );
        let event = received.lock().unwrap().take().unwrap();
        assert_eq!(event["protection"], "authenticate");
        assert_eq!(event["mime"], "image/png");
        assert_eq!(event["media"], true);
        assert_eq!(event["name"], "Protected image");
        assert!(!event.to_string().contains("private.png"));
    }

    #[test]
    fn reception_source_json_preserves_categories_and_omits_unknown() {
        use mesh_talk_core::node::{ReceivedDm, ReceivedVia};
        for (source, expected) in [
            (None, None),
            (Some(ReceivedVia::Sender), Some("sender")),
            (Some(ReceivedVia::Member), Some("member")),
            (Some(ReceivedVia::PostOffice), Some("post_office")),
            (Some(ReceivedVia::LinkedDevice), Some("linked_device")),
        ] {
            let event = dm_event(ReceivedDm {
                received_via: source,
                message_id: None,
                from_account: None,
                from: "sender".into(),
                from_name: "Sender".into(),
                text: Vec::new(),
                protection: None,
                reply_to: None,
            });
            let json = serde_json::to_value(event).unwrap();
            assert_eq!(json.get("received_via").and_then(|v| v.as_str()), expected);
            assert_eq!(json.get("received_via").is_none(), source.is_none());
        }
    }

    #[test]
    fn protected_dm_event_never_emits_plaintext() {
        let app = tauri::test::mock_app();
        let settings = SettingsState::default();
        settings.set(crate::settings::AppSettings {
            notifications: false,
            ..Default::default()
        });
        app.manage(settings);
        let received = Arc::new(Mutex::new(None));
        let sink = received.clone();
        app.listen(EVENT_DM_RECEIVED, move |event| {
            *sink.lock().unwrap() =
                Some(serde_json::from_str::<serde_json::Value>(event.payload()).unwrap());
        });
        emit_dm_received(
            app.handle(),
            mesh_talk_core::node::ReceivedDm {
                received_via: None,
                message_id: Some(mesh_talk_core::eventlog::EventId::new([3; 32])),
                from_account: None,
                from: "sender".into(),
                from_name: "Sender".into(),
                text: b"never emit this secret".to_vec(),
                protection: Some(mesh_talk_core::node::MessageProtection::Click),
                reply_to: None,
            },
        );
        let event = received.lock().unwrap().take().unwrap();
        assert_eq!(event["text"], "");
        assert_eq!(event["protection"], "click");
        assert!(!event.to_string().contains("never emit this secret"));
    }

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
            language: None,
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

#[derive(serde::Serialize, Clone)]
pub(crate) struct NodeStateChangedEvent {
    pub owner: String,
    pub account_id: String,
    pub device_id: String,
    pub kind: &'static str,
}

pub(crate) fn emit_node_state_changed<R: Runtime>(
    app: &tauri::AppHandle<R>,
    event: NodeStateChangedEvent,
) {
    if crate::reception_host::invalidate_identity_kind(
        app,
        &event.owner,
        &event.account_id,
        &event.device_id,
        event.kind,
    ) {
        return;
    }
    if crate::app_lock::app_locked(app) {
        return;
    }
    if let Err(error) = app.emit("node-state-changed", event) {
        log::warn!("Failed to emit node state invalidation: {error}");
    }
}

pub(crate) fn notify_reception<R: Runtime>(
    app: &tauri::AppHandle<R>,
    owner: &str,
    receipt: mesh_talk_core::node::HostReception,
    presentation: presentation::NotificationPresentation,
) {
    match receipt {
        mesh_talk_core::node::HostReception::DirectMessage(dm) => {
            publish_dm_received(app, dm, false, Some(presentation))
        }
        mesh_talk_core::node::HostReception::ChannelMessage(message) => {
            publish_channel_message(app, owner, message, false, Some(presentation))
        }
        mesh_talk_core::node::HostReception::File(file) => {
            publish_file_received(app, owner, file, false, Some(presentation))
        }
    }
}
