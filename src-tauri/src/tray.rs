use crate::services::node_service::NOTIFICATION_SERVICE;
use crate::state::AppState;
use lazy_static::lazy_static;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Wry;
use tauri::{AppHandle, Manager};

const MENU_SHOW: &str = "show";
const MENU_HIDE: &str = "hide";
const MENU_QUIT: &str = "quit";
const MENU_MARK_ALL_READ: &str = "mark_all_read";
const MENU_TOGGLE_NOTIFICATIONS: &str = "toggle_notifications";
const TRAY_ID: &str = "mesh-talk-tray";

// Global atomic counter for unread messages (kept for quick access)
static UNREAD_COUNT: AtomicU32 = AtomicU32::new(0);

type TrayMenuItem = MenuItem<Wry>;
type TrayCheckItem = CheckMenuItem<Wry>;

lazy_static! {
    static ref MARK_ALL_READ_ITEM: Mutex<Option<TrayMenuItem>> = Mutex::new(None);
    static ref TOGGLE_NOTIFICATIONS_ITEM: Mutex<Option<TrayCheckItem>> = Mutex::new(None);
}

/// Create the main tray icon and menu structure
pub fn create_system_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, MENU_SHOW, "Show", true, None::<&str>)?;
    let hide = MenuItem::with_id(app, MENU_HIDE, "Hide", true, None::<&str>)?;
    let mark_all_read = MenuItem::with_id(
        app,
        MENU_MARK_ALL_READ,
        "Mark All Read",
        false,
        None::<&str>,
    )?;
    let toggle_notifications = CheckMenuItem::with_id(
        app,
        MENU_TOGGLE_NOTIFICATIONS,
        "Disable Notifications",
        true,
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "Quit", true, None::<&str>)?;
    let separator1 = PredefinedMenuItem::separator(app)?;
    let separator2 = PredefinedMenuItem::separator(app)?;

    let tray_menu = Menu::with_items(
        app,
        &[
            &show,
            &hide,
            &separator1,
            &mark_all_read,
            &toggle_notifications,
            &separator2,
            &quit,
        ],
    )?;

    {
        let mut global_mark = MARK_ALL_READ_ITEM.lock().unwrap();
        *global_mark = Some(mark_all_read.clone());
    }
    {
        let mut global_toggle = TOGGLE_NOTIFICATIONS_ITEM.lock().unwrap();
        *global_toggle = Some(toggle_notifications.clone());
    }

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&tray_menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            MENU_QUIT => {
                app.exit(0);
            }
            MENU_HIDE => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }
            MENU_SHOW => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            MENU_MARK_ALL_READ => {
                mark_all_messages_read_from_tray(app);
            }
            MENU_TOGGLE_NOTIFICATIONS => {
                let enabled = NOTIFICATION_SERVICE.toggle_notifications();
                update_notification_menu(app, enabled);
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

    update_notification_menu(app, NOTIFICATION_SERVICE.is_enabled());
    update_mark_all_read_menu(app, false);

    Ok(())
}

/// Toggle window visibility
fn toggle_window_visibility(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        match window.is_visible() {
            Ok(true) => {
                let _ = window.hide();
            }
            Ok(false) => {
                let _ = window.show();
                let _ = window.set_focus();
            }
            Err(_) => {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }
    }
}

/// Update the unread message count in the system tray
pub fn update_unread_count(app: &AppHandle, count: u32) {
    UNREAD_COUNT.store(count, Ordering::Relaxed);

    let tooltip = if count > 0 {
        format!("Mesh-Talk ({})", count)
    } else {
        "Mesh-Talk".to_string()
    };

    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(&tooltip));
    }
    update_mark_all_read_menu(app, count > 0);

    // Set the badge label on macOS
    #[cfg(target_os = "macos")]
    {
        // Note: Tauri doesn't have a direct API for setting the dock badge label
        // This would require using a macOS-specific crate or FFI bindings
        // For now, we'll leave this as a placeholder
        let badge = if count > 0 {
            format!("{}", count)
        } else {
            "".to_string()
        };
        // This is a placeholder - actual implementation would require macOS-specific code
        println!("Setting macOS dock badge to: {}", badge);
    }
}

/// Update the notification toggle menu item title/state
pub fn update_notification_menu(_app: &AppHandle, enabled: bool) {
    let title = if enabled {
        "Disable Notifications"
    } else {
        "Enable Notifications"
    };

    if let Some(item) = TOGGLE_NOTIFICATIONS_ITEM.lock().unwrap().as_ref() {
        if let Err(e) = item.set_text(title) {
            eprintln!("Failed to update notification toggle text: {e}");
        }
        if let Err(e) = item.set_checked(enabled) {
            eprintln!("Failed to update notification toggle state: {e}");
        }
    }
}

/// Enable/disable the mark-all-read menu item
pub fn update_mark_all_read_menu(_app: &AppHandle, has_unread: bool) {
    if let Some(item) = MARK_ALL_READ_ITEM.lock().unwrap().as_ref() {
        if let Err(e) = item.set_enabled(has_unread) {
            eprintln!("Failed to update mark-all-read menu state: {e}");
        }
    }
}

fn mark_all_messages_read_from_tray(app: &AppHandle) {
    let Some(app_state) = app.try_state::<AppState>() else {
        return;
    };

    let Some(session) = app_state.session().get() else {
        return;
    };

    let message_service = app_state.message_service().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(e) = message_service.mark_all_read_for_user(session.user.id) {
            eprintln!("Failed to mark messages as read from tray: {:?}", e);
        }

        match message_service.count_unread_for_user(session.user.id) {
            Ok(count) => NOTIFICATION_SERVICE.set_unread_count(count),
            Err(e) => eprintln!("Failed to count unread messages: {:?}", e),
        }
    });
}

/// Get the current unread message count
pub fn get_unread_count() -> u32 {
    UNREAD_COUNT.load(Ordering::Relaxed)
}
