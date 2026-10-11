use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

const MENU_SHOW: &str = "show";
const MENU_HIDE: &str = "hide";
const MENU_QUIT: &str = "quit";
pub const TRAY_ID: &str = "mesh-talk-tray";

fn unread_tray_labels(count: u32) -> (Option<String>, String) {
    if count == 0 {
        return (None, "Mesh-Talk".to_string());
    }
    let title = if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    };
    let noun = if count == 1 { "message" } else { "messages" };
    (Some(title), format!("Mesh-Talk — {count} unread {noun}"))
}

/// Keep the menu bar count in sync with the dock/launcher badge. macOS displays the
/// title immediately beside the tray icon; its template icon stays legible in both themes.
pub fn set_unread_count(app: &AppHandle, count: u32) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let (title, tooltip) = unread_tray_labels(count);
        let _ = tray.set_title(title.as_deref());
        let _ = tray.set_tooltip(Some(tooltip));
    }
}

/// Create the main tray icon and menu (Show / Hide / Quit).
pub fn create_system_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, MENU_SHOW, "Show", true, None::<&str>)?;
    let hide = MenuItem::with_id(app, MENU_HIDE, "Hide", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;

    let tray_menu = Menu::with_items(app, &[&show, &hide, &separator, &quit])?;

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
    use super::unread_tray_labels;

    #[test]
    fn unread_count_is_visible_beside_the_menu_bar_icon() {
        assert_eq!(unread_tray_labels(0), (None, "Mesh-Talk".to_string()));
        assert_eq!(
            unread_tray_labels(1),
            (
                Some("1".to_string()),
                "Mesh-Talk — 1 unread message".to_string()
            )
        );
        assert_eq!(
            unread_tray_labels(3),
            (
                Some("3".to_string()),
                "Mesh-Talk — 3 unread messages".to_string()
            )
        );
        assert_eq!(
            unread_tray_labels(135),
            (
                Some("99+".to_string()),
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
