//! Fast regression guard for the IPC permissions used by the native window buttons.
//! Actual button behavior is exercised separately by native-window-diagnostics.yml.

fn assert_main_window_permission(permission: &str) {
    let capability: serde_json::Value =
        serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
    let windows = capability["windows"].as_array().unwrap();
    assert_eq!(windows, &[serde_json::json!("main")]);
    let permissions = capability["permissions"].as_array().unwrap();
    assert!(
        permissions.iter().any(|value| value == permission),
        "main window is missing {permission}; its custom control will be rejected by Tauri"
    );
}

#[test]
fn main_window_can_minimize() {
    assert_main_window_permission("core:window:allow-minimize");
}

#[test]
fn main_window_can_toggle_maximize() {
    assert_main_window_permission("core:window:allow-toggle-maximize");
}

#[test]
fn main_window_can_close() {
    assert_main_window_permission("core:window:allow-close");
}
