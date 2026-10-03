//! Preserve X11 placement during an in-process hide/show cycle, without changing
//! launch-time window-state persistence or compositor-controlled Wayland placement.
use tauri::Window;

#[cfg(target_os = "linux")]
use std::{collections::HashMap, sync::Mutex};
#[cfg(target_os = "linux")]
use tauri::{Manager, PhysicalPosition};

#[cfg(target_os = "linux")]
#[derive(Default)]
pub(crate) struct HiddenPositions(Mutex<HashMap<String, PhysicalPosition<i32>>>);

#[cfg(target_os = "linux")]
fn is_x11(window: &Window) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    matches!(
        window.window_handle().map(|handle| handle.as_raw()),
        Ok(RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_))
    )
}

pub(crate) fn hide(window: &Window) -> tauri::Result<()> {
    #[cfg(target_os = "linux")]
    if is_x11(window) && window.is_visible()? {
        // Repeated hides must not replace the remembered position with hidden
        // geometry. Do not reposition maximized, minimized or fullscreen windows.
        let position =
            if !window.is_maximized()? && !window.is_minimized()? && !window.is_fullscreen()? {
                window.outer_position().ok()
            } else {
                None
            };
        let state = window.state::<HiddenPositions>();
        let mut positions = state.0.lock().unwrap_or_else(|error| error.into_inner());
        positions.remove(window.label());
        if let Some(position) = position {
            positions.insert(window.label().to_owned(), position);
        }
    }
    window.hide()
}

pub(crate) fn show(window: &Window) -> tauri::Result<()> {
    #[cfg(target_os = "linux")]
    let position = if is_x11(window)
        && !window.is_visible()?
        && !window.is_maximized()?
        && !window.is_minimized()?
        && !window.is_fullscreen()?
    {
        let state = window.state::<HiddenPositions>();
        let position = state
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(window.label())
            .copied();
        position
    } else {
        None
    };
    window.show()?;
    #[cfg(target_os = "linux")]
    if let Some(position) = position {
        // Apply after mapping: the window manager can otherwise choose a fresh
        // placement. Tauri serializes these native requests on its event loop.
        window.set_position(position)?;
    }
    Ok(())
}
