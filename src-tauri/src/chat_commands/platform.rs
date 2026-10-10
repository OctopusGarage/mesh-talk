use crate::commands::CommandError;
use serde::Serialize;
use tauri::Manager;

/// Set the OS app-icon unread badge: the macOS dock number or Linux launcher count.
/// `count` is the total unread messages; 0 (or None) clears the badge. Best-effort — a
/// platform without stable count-badge support (notably Windows) intentionally no-ops.
#[tauri::command]
pub async fn set_badge(app: tauri::AppHandle, count: u32) -> Result<(), CommandError> {
    let label = if count == 0 {
        None
    } else {
        Some(count.to_string())
    };
    // The macOS dock badge is set via NSApp.dockTile (AppKit), which MUST run on the main
    // thread — but this async command runs on the Tauri runtime thread pool. Hop onto the
    // main thread to actually update the dock/taskbar badge.
    let handle = app.clone();
    app.run_on_main_thread(move || {
        #[cfg(target_os = "macos")]
        {
            let _ = &handle;
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
    })
    .map_err(|e| CommandError::Internal(format!("set_badge: {e}")))?;
    Ok(())
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
/// (System Settings ▸ Notifications ▸ <app> ▸ Badges). The app requests that authorization at
/// startup (see the frontend `ensureNotificationPermission`); without it this silently no-ops.
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
        if l.starts_with("SSID") && !l.starts_with("BSSID") {
            if let Some((_, v)) = l.split_once(':') {
                let v = v.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
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
/// - macOS: uses the built-in interactive `screencapture -i` area/window selector.
///   NOTE: macOS screen capture requires the "Screen Recording" permission (TCC). If it is
///   not granted, the produced PNG is blank/empty; the user must grant it in
///   System Settings → Privacy & Security → Screen Recording.
/// - Windows: captures the primary display through xcap.
/// - Linux: requests a display image from the desktop Screenshot portal.
/// All platforms return the capture to the shared select/annotate/send editor.
#[tauri::command]
pub async fn screenshot_available() -> bool {
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
        use ashpd::desktop::screenshot::{AvailableTargets, ScreenshotProxy};
        let Ok(proxy) = ScreenshotProxy::new().await else {
            return false;
        };
        proxy.version() < 3
            || proxy
                .available_targets()
                .await
                .is_ok_and(|targets| targets.contains(AvailableTargets::Screen))
    }
}

#[tauri::command]
pub async fn capture_screen(
    app: tauri::AppHandle,
    hide_window: bool,
) -> Result<Vec<u8>, CommandError> {
    let window = app.get_webview_window("main").map(|w| w.as_ref().window());

    if hide_window {
        if let Some(w) = &window {
            let _ = crate::window_visibility::hide(w);
        }
        // Give the compositor a moment to actually remove the window from the screen before
        // we capture, otherwise it can still be in the shot.
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
    }

    let result = capture_png().await.and_then(|bytes| {
        if bytes.is_empty() || valid_screenshot_png(&bytes) {
            Ok(bytes)
        } else {
            Err(CommandError::Internal(
                "screen capture returned an invalid PNG".into(),
            ))
        }
    });

    if hide_window {
        if let Some(w) = &window {
            let _ = crate::window_visibility::show(w);
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
    // The native selector lets the user choose a desktop region. The shared editor starts
    // with that region selected so annotation follows without a second drag.
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
            return Ok(Vec::new());
        }
        match std::fs::read(&tmp_clone) {
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

/// The native capture backend returns an image; selection and annotation stay in React.
#[cfg(target_os = "windows")]
async fn capture_png() -> Result<Vec<u8>, CommandError> {
    tokio::task::spawn_blocking(|| {
        let monitors = xcap::Monitor::all()
            .map_err(|e| CommandError::Internal(format!("cannot enumerate displays: {e}")))?;
        let monitor = monitors
            .iter()
            .find(|monitor| monitor.is_primary().unwrap_or(false))
            .or_else(|| monitors.first())
            .ok_or_else(|| CommandError::Internal("no display available for screenshot".into()))?;
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
    use ashpd::desktop::screenshot::{AvailableTargets, Screenshot, ScreenshotProxy};
    use ashpd::desktop::ResponseError;

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
            )))
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

fn valid_screenshot_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.get(12..16) == Some(b"IHDR")
}

#[cfg(test)]
mod screenshot_tests {
    use super::*;

    #[test]
    fn screenshot_bytes_must_be_a_png() {
        assert!(!valid_screenshot_png(&[]));
        assert!(!valid_screenshot_png(b"not a PNG"));
        assert!(valid_screenshot_png(&[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82,
        ]));
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
