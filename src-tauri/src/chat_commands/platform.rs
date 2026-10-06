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
/// - macOS: shells out to the built-in `screencapture -i` interactive region/window
///   selector, which blocks until the user selects an area or presses Esc.
///   NOTE: macOS screen capture requires the "Screen Recording" permission (TCC). If it is
///   not granted, the produced PNG is blank/empty; the user must grant it in
///   System Settings → Privacy & Security → Screen Recording.
/// - Windows/Linux: not wired up yet — returns a clear error (cross-platform capture is a
///   documented follow-up; see `capture_png`).
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

    let result = capture_png().await;

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
