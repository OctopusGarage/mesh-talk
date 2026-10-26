use crate::commands::CommandError;
use serde::Serialize;
use tauri::Manager;

/// Set the OS app-icon unread badge and the macOS menu bar count.
/// `count` is the total unread messages; 0 clears the badge. Best-effort — a
/// platform without stable count-badge support (notably Windows) intentionally no-ops.
#[tauri::command]
pub async fn set_badge(
    app: tauri::AppHandle,
    app_state: tauri::State<'_, crate::state::AppState>,
    owner: String,
    count: u32,
    revision: u64,
) -> Result<(), CommandError> {
    let session = app_state.session().clone();
    let lease = session.capture().map_err(CommandError::Authentication)?;
    if lease.owner() != owner {
        return Err(CommandError::Authentication("session replaced".into()));
    }
    let handle = app.clone();
    let badge_state = app_state.inner().clone();
    app.run_on_main_thread(move || {
        // The webview can dispatch this command just before logout. Never paint a
        // count belonging to a retired session, even if the same account signs in.
        let _ = session.matching(&lease, |_| {
            if badge_state.admit_badge_revision(lease.generation(), revision) {
                paint_badge(&handle, count);
            }
        });
    })
    .map_err(|e| CommandError::Internal(format!("set_badge: {e}")))?;
    Ok(())
}

/// Clear the badge after native logout, including when the frontend has already
/// unmounted. A later login invalidates this queued main-thread update.
pub(crate) fn clear_badge_after_logout<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    session: crate::state::SessionState,
) {
    let generation = session.generation();
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let _ = session.unchanged(generation, || paint_badge(&handle, 0));
    });
}

fn paint_badge<R: tauri::Runtime>(handle: &tauri::AppHandle<R>, count: u32) {
    let label = if count == 0 {
        None
    } else {
        Some(count.to_string())
    };
    crate::tray::set_unread_count(handle, count);
    #[cfg(target_os = "macos")]
    {
        let _ = handle;
        set_dock_badge(label);
    }
    #[cfg(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    {
        let n = label.and_then(|s| s.parse::<i64>().ok());
        if let Some(window) = handle.get_webview_window("main") {
            let _ = window.set_badge_count(n);
        }
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )))]
    {
        let _ = (handle, label);
    }
}

/// Set (or clear, with `None`) the macOS dock-icon badge label directly via AppKit.
///
/// Why not `WebviewWindow::set_badge_count`? tao's macOS implementation does
/// `NSApp.dockTile.setBadgeLabel:` but never calls `[dockTile display]`. In a bundled `.app`
/// the badge then silently fails to repaint (the call returns fine, nothing appears). We also
/// resolve the app via `NSApplication.sharedApplication` (reliable) rather than the global
/// `NSApp` (nil until first set), and force a `display()` so the number actually shows.
///
/// NOTE: macOS only renders the dock badge if the app is authorized for notification badges
/// (System Settings ▸ Notifications ▸ <app> ▸ Badges). The app requests that authorization through
/// the explicit notification-enable action; without it this silently no-ops.
///
/// MUST be called on the main thread (AppKit requirement); callers hop via `run_on_main_thread`.
#[cfg(target_os = "macos")]
pub(crate) fn set_dock_badge(label: Option<String>) {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use std::ffi::CString;
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return;
        }
        let dock_tile: *mut AnyObject = msg_send![app, dockTile];
        if dock_tile.is_null() {
            return;
        }
        let ns_label: *mut AnyObject = match label.as_deref() {
            Some(s) => {
                let c = CString::new(s).unwrap_or_default();
                msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()]
            }
            None => std::ptr::null_mut(),
        };
        let _: () = msg_send![dock_tile, setBadgeLabel: ns_label];
        let _: () = msg_send![dock_tile, display];
    }
}

/// The name (SSID) of the Wi-Fi network this machine is on, or `None` if it can't be
/// determined (wired, no Wi-Fi, or the OS withholds it). Mesh-Talk is LAN-scoped, so the UI
/// shows this to make "which network am I reachable on" obvious. Best-effort + platform-
/// specific; never errors on a missing tool.
#[tauri::command]
pub async fn network_name() -> Result<Option<String>, CommandError> {
    tokio::task::spawn_blocking(current_ssid)
        .await
        .map_err(|e| CommandError::Internal(format!("network_name: {e}")))
}

#[cfg(target_os = "macos")]
fn current_ssid() -> Option<String> {
    // `networksetup -getairportnetwork` is blocked without Location on recent macOS, but
    // `system_profiler SPAirPortDataType` still reports the joined network: under a
    // "Current Network Information:" line, the next line is "<SSID>:".
    let out = std::process::Command::new("system_profiler")
        .arg("SPAirPortDataType")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.trim() == "Current Network Information:" {
            let ssid = lines.next()?.trim().trim_end_matches(':').trim();
            return (!ssid.is_empty()).then(|| ssid.to_string());
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn current_ssid() -> Option<String> {
    use std::os::windows::process::CommandExt;

    // Piped output alone does not suppress a console in a Windows GUI process.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("netsh")
        .args(["wlan", "show", "interfaces"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let l = line.trim();
        // Match "SSID                   : Name" but not "BSSID".
        if l.starts_with("SSID")
            && !l.starts_with("BSSID")
            && let Some((_, v)) = l.split_once(':')
        {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn current_ssid() -> Option<String> {
    if let Ok(out) = std::process::Command::new("nmcli")
        .args(["-t", "-f", "active,ssid", "dev", "wifi"])
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("yes:") {
                let s = rest.trim();
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
        }
    }
    if let Ok(out) = std::process::Command::new("iwgetid").arg("-r").output() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn current_ssid() -> Option<String> {
    None
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
/// - macOS: captures the app window's monitor with `screencapture -x -R`.
///   NOTE: macOS screen capture requires the "Screen Recording" permission (TCC). If it is
///   not granted, the produced PNG is blank/empty; the user must grant it in
///   System Settings → Privacy & Security → Screen Recording.
/// - Windows: captures the app window's monitor through xcap.
/// - Linux: uses the Screenshot portal, or an optional XFCE helper on native X11
///   when the portal cannot provide screen capture.
/// All platforms return the capture to the shared select/annotate/send editor.
#[tauri::command]
pub async fn screenshot_available(_app: tauri::AppHandle) -> bool {
    #[cfg(target_os = "macos")]
    {
        true // The OS can prompt for Screen Recording permission when needed.
    }
    #[cfg(target_os = "windows")]
    {
        tokio::task::spawn_blocking(|| {
            xcap::Monitor::all().is_ok_and(|monitors| !monitors.is_empty())
        })
        .await
        .unwrap_or(false)
    }
    #[cfg(target_os = "linux")]
    {
        linux_screenshot_backend(&_app).await.is_ok()
    }
}

#[tauri::command]
pub async fn capture_screen<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    hide_window: bool,
) -> Result<Vec<u8>, CommandError> {
    let authority = crate::screenshot_shortcuts::CaptureAuthority::capture(
        &app.state::<crate::state::AppState>(),
    )?;
    let window = app.get_webview_window("main").map(|w| w.as_ref().window());

    // Resolve once before hiding. Never switch backends after a portal denial or cancel.
    #[cfg(target_os = "linux")]
    let backend = linux_screenshot_backend(&app).await?;

    // Resolve the target before hiding: capture and the later overlay use the same display.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let monitor = window
        .as_ref()
        .ok_or_else(|| CommandError::Internal("screenshot window unavailable".into()))?
        .current_monitor()
        .map_err(|e| CommandError::Internal(format!("cannot query screenshot display: {e}")))?
        .ok_or_else(|| CommandError::Internal("screenshot display unavailable".into()))?;

    let hide = set_capture_window_hidden(&app, &authority, window.clone(), true);
    let restore = set_capture_window_hidden(&app, &authority, window, false);
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let captured = capture_png(monitor);
    #[cfg(target_os = "linux")]
    let captured = super::linux_screenshot::capture(backend, capture_png());
    capture_for_session(&authority, hide_window, hide, captured, restore).await
}

async fn set_capture_window_hidden<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    authority: &crate::screenshot_shortcuts::CaptureAuthority,
    window: Option<tauri::Window<R>>,
    hidden: bool,
) -> Result<(), CommandError> {
    authority
        .on_main_thread(app, move || {
            if let Some(window) = window {
                if hidden {
                    let _ = crate::window_visibility::hide(&window);
                } else {
                    let _ = window.unminimize();
                    let _ = crate::window_visibility::show(&window);
                    let _ = window.set_focus();
                }
            }
        })
        .await
}

/// Keep the acquisition/restore lifetime independent of the OS pixel provider.
/// The native futures authorize again when their main-thread commits execute.
async fn capture_for_session(
    authority: &crate::screenshot_shortcuts::CaptureAuthority,
    hide_window: bool,
    hide: impl std::future::Future<Output = Result<(), CommandError>>,
    captured: impl std::future::Future<Output = Result<Vec<u8>, CommandError>>,
    restore: impl std::future::Future<Output = Result<(), CommandError>>,
) -> Result<Vec<u8>, CommandError> {
    if hide_window {
        hide.await?;
        // Let the compositor remove the owned window before acquiring pixels.
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
    }
    authority.ensure_current()?;
    let result = captured.await.and_then(|bytes| {
        if bytes.is_empty() || valid_screenshot_png(&bytes) {
            Ok(bytes)
        } else {
            Err(CommandError::Internal(
                "screen capture returned an invalid PNG".into(),
            ))
        }
    });

    if hide_window {
        restore.await?;
    }
    authority.ensure_current()?;

    result
}

#[cfg(target_os = "linux")]
pub(super) async fn linux_screenshot_backend<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<super::linux_screenshot::Backend, CommandError> {
    let x11 = app
        .get_webview_window("main")
        .is_some_and(|window| crate::window_visibility::is_x11(&window.as_ref().window()));
    super::linux_screenshot::resolve_backend(
        super::linux_screenshot::portal_available(),
        x11,
        super::linux_screenshot::find_x11_tool(),
    )
    .await
}

/// Platform-specific capture, returning PNG bytes (empty = user cancelled).
/// macOS Screen Recording permission (TCC) check + request, via CoreGraphics. Without this
/// permission, `screencapture` silently returns only the DESKTOP wallpaper (window content is
/// blanked) — so we must verify it up front rather than hand back a confusing desktop shot.
#[cfg(target_os = "macos")]
mod screen_recording {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
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
async fn capture_png(monitor: tauri::Monitor) -> Result<Vec<u8>, CommandError> {
    // Without the Screen Recording permission, screencapture would just grab the desktop
    // wallpaper. Verify first; if missing, prompt/register the app and bail with a clear
    // signal so the UI tells the user to grant it (and restart) — not a desktop screenshot.
    if !screen_recording::has_access() {
        screen_recording::request_access();
        return Err(CommandError::Internal(SCREEN_PERMISSION_ERR.into()));
    }
    // Tauri monitor positions and sizes are physical; screencapture regions use logical coordinates.
    let rectangle = mac_capture_rectangle(
        monitor.position().x,
        monitor.position().y,
        monitor.size().width,
        monitor.size().height,
        monitor.scale_factor(),
    );
    // Selection and annotation share one frozen capture of the app's current display.
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>, CommandError> {
        let tmp = tempfile::Builder::new()
            .prefix("mesh-talk-shot-")
            .suffix(".png")
            .tempfile()
            .map_err(|e| CommandError::Internal(format!("cannot create screenshot file: {e}")))?;
        let status = std::process::Command::new("screencapture")
            .args(["-x", "-R", &rectangle])
            .arg(tmp.path())
            .status()
            .map_err(|e| CommandError::Internal(format!("screencapture failed: {e}")))?;
        if !status.success() {
            return Err(CommandError::Internal("screencapture failed".into()));
        }
        std::fs::read(tmp.path())
            .map_err(|e| CommandError::Internal(format!("cannot read screenshot: {e}")))
    })
    .await
    .map_err(|e| CommandError::Internal(format!("capture task failed: {e}")))?
}

#[cfg(target_os = "macos")]
fn mac_capture_rectangle(x: i32, y: i32, width: u32, height: u32, scale: f64) -> String {
    format!(
        "{},{},{},{}",
        (f64::from(x) / scale).round() as i64,
        (f64::from(y) / scale).round() as i64,
        (f64::from(width) / scale).round() as u64,
        (f64::from(height) / scale).round() as u64
    )
}

/// The native capture backend returns an image; selection and annotation stay in React.
#[cfg(target_os = "windows")]
async fn capture_png(monitor: tauri::Monitor) -> Result<Vec<u8>, CommandError> {
    tokio::task::spawn_blocking(move || {
        let monitors = xcap::Monitor::all()
            .map_err(|e| CommandError::Internal(format!("cannot enumerate displays: {e}")))?;
        let monitor = monitors
            .iter()
            .find(|candidate| {
                candidate.x().ok() == Some(monitor.position().x)
                    && candidate.y().ok() == Some(monitor.position().y)
                    && candidate.width().ok() == Some(monitor.size().width)
                    && candidate.height().ok() == Some(monitor.size().height)
            })
            .ok_or_else(|| {
                CommandError::Internal("screenshot display no longer available".into())
            })?;
        let pixels = monitor
            .capture_image()
            .map_err(|e| CommandError::Internal(format!("cannot capture display: {e}")))?;
        let mut output = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(pixels)
            .write_to(&mut output, image::ImageFormat::Png)
            .map_err(|e| CommandError::Internal(format!("cannot encode screenshot: {e}")))?;
        Ok(output.into_inner())
    })
    .await
    .map_err(|e| CommandError::Internal(format!("capture task failed: {e}")))?
}

#[cfg(target_os = "linux")]
fn portal_file_path(uri: &str) -> Result<std::path::PathBuf, CommandError> {
    url::Url::parse(uri)
        .map_err(|e| CommandError::Internal(format!("invalid screenshot URI: {e}")))?
        .to_file_path()
        .map_err(|_| CommandError::Internal("screenshot portal returned a non-local file".into()))
}

#[cfg(target_os = "linux")]
async fn capture_png() -> Result<Vec<u8>, CommandError> {
    use ashpd::desktop::ResponseError;
    use ashpd::desktop::screenshot::{AvailableTargets, Screenshot, ScreenshotProxy};

    let proxy = ScreenshotProxy::new()
        .await
        .map_err(|e| CommandError::Internal(format!("screenshot portal unavailable: {e}")))?;
    let mut request = Screenshot::request().interactive(true).modal(false);
    if proxy.version() >= 3 {
        let targets = proxy
            .available_targets()
            .await
            .map_err(|e| CommandError::Internal(format!("cannot query screenshot targets: {e}")))?;
        if targets.contains(AvailableTargets::Screen) {
            request = request.target(AvailableTargets::Screen);
        } else {
            return Err(CommandError::Internal(
                "screenshot portal cannot capture a display".into(),
            ));
        }
    }
    let response = request.send().await.and_then(|request| request.response());
    let shot = match response {
        Ok(shot) => shot,
        Err(ashpd::Error::Response(ResponseError::Cancelled)) => return Ok(Vec::new()),
        Err(e) => {
            return Err(CommandError::Internal(format!(
                "screenshot portal failed: {e}"
            )));
        }
    };
    let path = portal_file_path(shot.uri().as_str())?;
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|e| CommandError::Internal(format!("cannot access portal screenshot: {e}")))?;
    if metadata.len() > 128 * 1024 * 1024 {
        return Err(CommandError::Internal("screenshot exceeds 128 MB".into()));
    }
    tokio::fs::read(path)
        .await
        .map_err(|e| CommandError::Internal(format!("cannot read portal screenshot: {e}")))
}

pub(super) fn valid_screenshot_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.get(12..16) == Some(b"IHDR")
}

#[cfg(test)]
mod screenshot_tests {
    use super::*;
    use crate::screenshot_shortcuts::CaptureAuthority;
    use crate::state::AppState;
    use std::sync::{Arc, Mutex};

    const PNG: &[u8] = &[137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82];

    fn capture_host() -> (
        tempfile::TempDir,
        tauri::App<tauri::test::MockRuntime>,
        AppState,
        crate::services::user::User,
    ) {
        let root = tempfile::tempdir().unwrap();
        let service = crate::services::auth_service::AuthService::new(Arc::new(
            mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
            ),
        ));
        let state = AppState::new(service);
        let user = crate::services::user::User {
            user_id: "capture-owner".into(),
            account_login_name: None,
            name: "capture".into(),
            display_name: "Capture".into(),
            address: "local".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        };
        state
            .session()
            .publish("capture-session".into(), user.clone(), "pw".into());
        let builder = tauri::test::mock_builder()
            .manage(state.clone())
            .manage(state.app_lock().clone());
        #[cfg(target_os = "linux")]
        let builder = builder.manage(crate::window_visibility::HiddenPositions::default());
        let host = builder
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        (root, host, state, user)
    }

    #[tokio::test]
    async fn capture_rejects_signed_out_and_locked_sessions_before_native_setup() {
        let (_root, host, state, user) = capture_host();
        assert!(state.session().logout("capture-session").is_ok());
        assert!(matches!(
            Box::pin(capture_screen(host.handle().clone(), true)).await,
            Err(CommandError::Authorization(_))
        ));
        state
            .session()
            .publish("replacement".into(), user, "pw".into());
        crate::app_lock::lock_app(host.state(), host.state()).unwrap();
        assert!(matches!(
            Box::pin(capture_screen(host.handle().clone(), false)).await,
            Err(CommandError::Authorization(_))
        ));
    }

    #[tokio::test]
    async fn hidden_capture_restores_after_success_cancel_backend_error_and_invalid_pixels() {
        for outcome in ["image", "cancel", "backend error", "invalid image"] {
            let (_root, _host, state, _user) = capture_host();
            let authority = CaptureAuthority::capture(&state).unwrap();
            let trace = Mutex::new(Vec::new());
            let result = capture_for_session(
                &authority,
                true,
                async { authority.with_current(|| trace.lock().unwrap().push("hide")) },
                async {
                    trace.lock().unwrap().push("capture");
                    match outcome {
                        "image" => Ok(PNG.to_vec()),
                        "cancel" => Ok(Vec::new()),
                        "backend error" => {
                            Err(CommandError::Internal("provider unavailable".into()))
                        }
                        _ => Ok(b"invalid provider pixels".to_vec()),
                    }
                },
                async { authority.with_current(|| trace.lock().unwrap().push("restore")) },
            )
            .await;
            match outcome {
                "image" => assert!(result.is_ok_and(|pixels| pixels == PNG)),
                "cancel" => assert!(result.is_ok_and(|pixels| pixels.is_empty())),
                "backend error" => assert!(
                    matches!(result, Err(CommandError::Internal(error)) if error == "provider unavailable")
                ),
                _ => assert!(
                    matches!(result, Err(CommandError::Internal(error)) if error == "screen capture returned an invalid PNG")
                ),
            }
            assert_eq!(*trace.lock().unwrap(), ["hide", "capture", "restore"]);
        }
    }

    #[tokio::test]
    async fn visible_capture_leaves_window_futures_unpolled() {
        let (_root, _host, state, _user) = capture_host();
        let authority = CaptureAuthority::capture(&state).unwrap();
        let result = capture_for_session(
            &authority,
            false,
            async { Err(CommandError::Internal("unexpected hide".into())) },
            async { Ok(PNG.to_vec()) },
            async { Err(CommandError::Internal("unexpected restore".into())) },
        )
        .await;
        assert!(result.is_ok_and(|pixels| pixels == PNG));
    }

    #[tokio::test]
    async fn capture_drops_pixels_and_retired_restoration_after_backend_wait() {
        for replace_owner in [false, true] {
            let (_root, _host, state, user) = capture_host();
            let authority = CaptureAuthority::capture(&state).unwrap();
            let trace = Mutex::new(Vec::new());
            let result = capture_for_session(
                &authority,
                true,
                async { authority.with_current(|| trace.lock().unwrap().push("hide")) },
                async {
                    trace.lock().unwrap().push("capture");
                    if replace_owner {
                        state
                            .session()
                            .publish("replacement".into(), user, "pw".into());
                    } else {
                        state.app_lock().clear();
                    }
                    Ok(PNG.to_vec())
                },
                async { authority.with_current(|| trace.lock().unwrap().push("restore")) },
            )
            .await;
            assert!(matches!(result, Err(CommandError::Authorization(_))));
            assert_eq!(*trace.lock().unwrap(), ["hide", "capture"]);
        }
    }

    #[tokio::test]
    async fn capture_rechecks_after_hide_and_after_restore() {
        for retire_after_hide in [true, false] {
            let (_root, _host, state, _user) = capture_host();
            let authority = CaptureAuthority::capture(&state).unwrap();
            let trace = Mutex::new(Vec::new());
            let result = capture_for_session(
                &authority,
                true,
                async {
                    authority.with_current(|| trace.lock().unwrap().push("hide"))?;
                    if retire_after_hide {
                        state.app_lock().clear();
                    }
                    Ok(())
                },
                async {
                    trace.lock().unwrap().push("capture");
                    Ok(PNG.to_vec())
                },
                async {
                    authority.with_current(|| trace.lock().unwrap().push("restore"))?;
                    state.app_lock().clear();
                    Ok(())
                },
            )
            .await;
            assert!(matches!(result, Err(CommandError::Authorization(_))));
            let expected = if retire_after_hide {
                vec!["hide"]
            } else {
                vec!["hide", "capture", "restore"]
            };
            assert_eq!(*trace.lock().unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn capture_surfaces_window_dispatch_failures_without_publishing_pixels() {
        for hide_fails in [true, false] {
            let (_root, _host, state, _user) = capture_host();
            let authority = CaptureAuthority::capture(&state).unwrap();
            let trace = Mutex::new(Vec::new());
            let result = capture_for_session(
                &authority,
                true,
                async {
                    trace.lock().unwrap().push("hide");
                    if hide_fails {
                        Err(CommandError::Internal("dispatch unavailable".into()))
                    } else {
                        Ok(())
                    }
                },
                async {
                    trace.lock().unwrap().push("capture");
                    Ok(PNG.to_vec())
                },
                async {
                    trace.lock().unwrap().push("restore");
                    Err(CommandError::Internal("dispatch unavailable".into()))
                },
            )
            .await;
            assert!(
                matches!(result, Err(CommandError::Internal(error)) if error == "dispatch unavailable")
            );
            let expected = if hide_fails {
                vec!["hide"]
            } else {
                vec!["hide", "capture", "restore"]
            };
            assert_eq!(*trace.lock().unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn native_window_commits_require_the_captured_session() {
        let (_root, host, state, user) = capture_host();
        let authority = CaptureAuthority::capture(&state).unwrap();
        let window = tauri::WebviewWindowBuilder::new(
            &host,
            "main",
            tauri::WebviewUrl::App("index.html".into()),
        )
        .build()
        .unwrap()
        .as_ref()
        .window();
        for hidden in [true, false] {
            set_capture_window_hidden(host.handle(), &authority, Some(window.clone()), hidden)
                .await
                .unwrap();
            set_capture_window_hidden(host.handle(), &authority, None, hidden)
                .await
                .unwrap();
        }
        state
            .session()
            .publish("replacement".into(), user, "pw".into());
        assert!(matches!(
            set_capture_window_hidden(host.handle(), &authority, Some(window), false).await,
            Err(CommandError::Authorization(_))
        ));
    }

    #[test]
    fn screenshot_bytes_must_be_a_png() {
        assert!(!valid_screenshot_png(&[]));
        assert!(!valid_screenshot_png(b"not a PNG"));
        assert!(valid_screenshot_png(&[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82,
        ]));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn capture_rectangle_tracks_the_windows_monitor_in_logical_coordinates() {
        assert_eq!(
            mac_capture_rectangle(-2880, 0, 2880, 1800, 2.0),
            "-1440,0,1440,900"
        );
        assert_eq!(
            mac_capture_rectangle(0, 2160, 1920, 1080, 1.0),
            "0,2160,1920,1080"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn portal_result_must_be_a_local_file_uri() {
        assert_eq!(
            portal_file_path("file:///tmp/a%20b.png").unwrap(),
            std::path::PathBuf::from("/tmp/a b.png")
        );
        assert!(portal_file_path("https://example.com/a.png").is_err());
    }
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
