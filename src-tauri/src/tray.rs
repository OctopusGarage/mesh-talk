use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

const MENU_SHOW: &str = "show";
const MENU_HIDE: &str = "hide";
const MENU_QUIT: &str = "quit";
const MENU_UNREAD: &str = "unread";
const MENU_DIAGNOSTICS: &str = "diagnostics";
const MENU_SETTINGS: &str = "settings";
const MENU_ABOUT: &str = "about";
pub const TRAY_ID: &str = "mesh-talk-tray";

struct TrayMenuState<R: tauri::Runtime> {
    unread: MenuItem<R>,
}

fn unread_tray_labels(count: u32) -> (String, String) {
    if count == 0 {
        return (String::new(), "Mesh-Talk".to_string());
    }
    let title = if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    };
    let noun = if count == 1 { "message" } else { "messages" };
    (title, format!("Mesh-Talk — {count} unread {noun}"))
}

fn unread_menu_label(count: u32) -> String {
    match count {
        0 => "No unread messages".to_string(),
        1 => "1 unread message".to_string(),
        _ => format!("{count} unread messages"),
    }
}

/// Keep the menu bar count in sync with the dock/launcher badge. macOS displays the
/// title immediately beside the tray icon; its template icon stays legible in both themes.
pub fn set_unread_count<R: tauri::Runtime>(app: &AppHandle<R>, count: u32) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let (title, tooltip) = unread_tray_labels(count);
        // tray-icon 0.24.1 leaves the existing macOS title untouched for None.
        // An explicit empty string clears a previously painted unread count.
        let _ = tray.set_title(Some(title.as_str()));
        let _ = tray.set_tooltip(Some(tooltip));
        if let Some(menu) = app.try_state::<TrayMenuState<R>>() {
            let _ = menu.unread.set_text(unread_menu_label(count));
        }
    }
}

/// Create the main tray icon and its native, keyboard-accessible menu.
pub fn create_system_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, MENU_SHOW, "Open Mesh-Talk", true, None::<&str>)?;
    let unread = MenuItem::with_id(app, MENU_UNREAD, unread_menu_label(0), false, None::<&str>)?;
    let diagnostics = MenuItem::with_id(
        app,
        MENU_DIAGNOSTICS,
        "Connection Diagnostics…",
        true,
        None::<&str>,
    )?;
    let settings = MenuItem::with_id(app, MENU_SETTINGS, "Settings…", true, None::<&str>)?;
    let about = MenuItem::with_id(app, MENU_ABOUT, "About Mesh-Talk", true, None::<&str>)?;
    let hide = MenuItem::with_id(app, MENU_HIDE, "Hide", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "Quit Mesh-Talk", true, None::<&str>)?;

    let tray_menu = Menu::with_items(
        app,
        &[
            &show,
            &unread,
            &PredefinedMenuItem::separator(app)?,
            &diagnostics,
            &settings,
            &about,
            &PredefinedMenuItem::separator(app)?,
            &hide,
            &quit,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&tray_menu)
        .icon_as_template(cfg!(target_os = "macos"))
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            MENU_QUIT => app.exit(0),
            MENU_HIDE => {
                if let Some(window) = app.get_webview_window("main").map(|w| w.as_ref().window()) {
                    let _ = crate::window_visibility::hide(&window);
                }
            }
            MENU_SHOW => {
                if let Some(window) = app.get_webview_window("main").map(|w| w.as_ref().window()) {
                    let _ = crate::window_visibility::show(&window);
                    let _ = window.set_focus();
                }
            }
            MENU_DIAGNOSTICS | MENU_SETTINGS | MENU_ABOUT => {
                if let Some(window) = app.get_webview_window("main").map(|w| w.as_ref().window()) {
                    let _ = crate::window_visibility::show(&window);
                    let _ = window.set_focus();
                    let _ = app.emit("tray-navigate", event.id.as_ref());
                }
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_window_visibility(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder.tooltip("Mesh-Talk").build(app)?;
    app.manage(TrayMenuState { unread });
    Ok(())
}

/// Toggle the main window's visibility.
fn toggle_window_visibility(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main").map(|w| w.as_ref().window()) {
        match window.is_visible() {
            Ok(true) => {
                let _ = crate::window_visibility::hide(&window);
            }
            _ => {
                let _ = crate::window_visibility::show(&window);
                let _ = window.set_focus();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{unread_menu_label, unread_tray_labels};

    #[test]
    fn unread_menu_has_a_clear_empty_state_and_singular_count() {
        assert_eq!(unread_menu_label(0), "No unread messages");
        assert_eq!(unread_menu_label(1), "1 unread message");
        assert_eq!(unread_menu_label(120), "120 unread messages");
    }

    #[test]
    fn unread_count_is_visible_beside_the_menu_bar_icon() {
        assert_eq!(
            unread_tray_labels(0),
            (String::new(), "Mesh-Talk".to_string())
        );
        assert_eq!(
            unread_tray_labels(1),
            ("1".to_string(), "Mesh-Talk — 1 unread message".to_string())
        );
        assert_eq!(
            unread_tray_labels(3),
            ("3".to_string(), "Mesh-Talk — 3 unread messages".to_string())
        );
        assert_eq!(
            unread_tray_labels(135),
            (
                "99+".to_string(),
                "Mesh-Talk — 135 unread messages".to_string()
            )
        );
    }

    #[test]
    fn startup_has_no_unhandled_config_tray_icon() {
        let context: tauri::Context<tauri::Wry> = tauri::generate_context!();
        assert!(
            context.config().app.tray_icon.is_none(),
            "Tauri auto-registers app.trayIcon before the handled tray is created"
        );
    }
}
