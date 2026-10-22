//! Machine notification authorization. The desktop plugin's permission API is
//! an unconditional grant; it is not evidence of current OS authorization.
use crate::commands::CommandError;
use serde::Serialize;
use tauri_plugin_opener::OpenerExt;

// Some statuses are observable only on macOS; retain one stable IPC vocabulary.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PermissionStatus {
    Granted,
    Denied,
    Prompt,
    Limited,
    Unknown,
}

#[derive(Serialize)]
pub struct NotificationPermission {
    status: PermissionStatus,
    platform: &'static str,
    can_open_settings: bool,
}

fn snapshot(status: PermissionStatus) -> NotificationPermission {
    NotificationPermission {
        status,
        platform: if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(target_os = "windows") {
            "windows"
        } else if cfg!(target_os = "linux") {
            "linux"
        } else {
            "other"
        },
        can_open_settings: settings_url().is_some(),
    }
}

fn settings_url() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("x-apple.systempreferences:com.apple.Notifications-Settings.extension")
    } else if cfg!(target_os = "windows") {
        Some("ms-settings:notifications")
    } else {
        None
    }
}

#[tauri::command]
pub async fn notification_permission(app: tauri::AppHandle) -> NotificationPermission {
    observe(app, false).await
}

#[tauri::command]
pub async fn request_notification_permission(app: tauri::AppHandle) -> NotificationPermission {
    observe(app, true).await
}

async fn observe(app: tauri::AppHandle, request: bool) -> NotificationPermission {
    let status = tauri::async_runtime::spawn_blocking(move || platform_permission(&app, request))
        .await
        .unwrap_or(PermissionStatus::Unknown);
    snapshot(status)
}

/// Only fixed OS destinations are accepted; opening them is an explicit UI action.
#[tauri::command]
pub async fn open_notification_settings(app: tauri::AppHandle) -> Result<(), CommandError> {
    let url = settings_url().ok_or_else(|| {
        CommandError::Internal("Open your desktop notification settings manually".into())
    })?;
    app.opener().open_url(url, None::<&str>).map_err(|error| {
        CommandError::Internal(format!("Cannot open notification settings: {error}"))
    })
}

#[cfg(target_os = "macos")]
fn mac_status(authorization: isize, alerts: isize) -> PermissionStatus {
    match authorization {
        0 => PermissionStatus::Prompt,
        1 => PermissionStatus::Denied,
        2 | 4 if alerts == 2 => PermissionStatus::Granted,
        2..=4 => PermissionStatus::Limited,
        _ => PermissionStatus::Unknown,
    }
}

#[cfg(target_os = "macos")]
fn platform_permission(app: &tauri::AppHandle, request: bool) -> PermissionStatus {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSBundle, NSError};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNNotificationSettings, UNUserNotificationCenter,
    };
    use std::{ptr::NonNull, sync::mpsc, time::Duration};

    // currentNotificationCenter raises an Objective-C exception for a bare
    // development binary. The delivery plugin also uses Terminal's identity in
    // dev, so never claim the dev process represents the installed application.
    let bundle = NSBundle::mainBundle();
    if tauri::is_dev()
        || bundle
            .bundleIdentifier()
            .is_none_or(|id| id.to_string() != app.config().identifier)
    {
        return PermissionStatus::Unknown;
    }
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let read = || {
        let (sender, receiver) = mpsc::sync_channel(1);
        let reply = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            // Apple's callback owns the settings for the duration of this call.
            let settings = unsafe { settings.as_ref() };
            let _ = sender.send(mac_status(
                settings.authorizationStatus().0,
                settings.alertSetting().0,
            ));
        });
        center.getNotificationSettingsWithCompletionHandler(&reply);
        receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or(PermissionStatus::Unknown)
    };
    let status = read();
    if !request || status != PermissionStatus::Prompt {
        return status;
    }
    let (sender, receiver) = mpsc::sync_channel(1);
    let reply = RcBlock::new(move |_granted: Bool, error: *mut NSError| {
        let _ = sender.send(error.is_null());
    });
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert
            | UNAuthorizationOptions::Sound
            | UNAuthorizationOptions::Badge,
        &reply,
    );
    if receiver.recv_timeout(Duration::from_secs(120)) != Ok(true) {
        return PermissionStatus::Unknown;
    }
    read()
}

#[cfg(target_os = "windows")]
fn platform_permission(app: &tauri::AppHandle, _request: bool) -> PermissionStatus {
    use windows::{
        UI::Notifications::{NotificationSetting, ToastNotificationManager},
        core::HSTRING,
    };
    // The notification plugin uses a fallback identity for uninstalled target
    // binaries. Only observe the real app identity in an installed application.
    let Ok(exe) = tauri::utils::platform::current_exe() else {
        return PermissionStatus::Unknown;
    };
    if exe
        .parent()
        .is_none_or(|parent| parent.ends_with("target/debug") || parent.ends_with("target/release"))
    {
        return PermissionStatus::Unknown;
    }
    let setting = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(
        &app.config().identifier,
    ))
    .and_then(|notifier| notifier.Setting());
    match setting {
        Ok(NotificationSetting::Enabled) => PermissionStatus::Granted,
        Ok(
            NotificationSetting::DisabledForApplication
            | NotificationSetting::DisabledForUser
            | NotificationSetting::DisabledByGroupPolicy
            | NotificationSetting::DisabledByManifest,
        ) => PermissionStatus::Denied,
        _ => PermissionStatus::Unknown,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_permission(_app: &tauri::AppHandle, _request: bool) -> PermissionStatus {
    // Linux notification daemons have no portable per-app permission query or
    // settings URI. A successful DBus connection cannot prove alerts are enabled.
    PermissionStatus::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_is_serialized_distinctly_from_denied() {
        assert_eq!(
            serde_json::to_value(snapshot(PermissionStatus::Unknown)).unwrap()["status"],
            "unknown"
        );
        assert_eq!(
            serde_json::to_value(snapshot(PermissionStatus::Denied)).unwrap()["status"],
            "denied"
        );
    }

    #[test]
    fn settings_action_matches_supported_platforms() {
        assert_eq!(
            settings_url().is_some(),
            cfg!(any(target_os = "macos", target_os = "windows"))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_permission_distinguishes_prompts_denial_and_quiet_alerts() {
        assert_eq!(mac_status(0, 0), PermissionStatus::Prompt);
        assert_eq!(mac_status(1, 1), PermissionStatus::Denied);
        assert_eq!(mac_status(2, 2), PermissionStatus::Granted);
        assert_eq!(mac_status(2, 1), PermissionStatus::Limited);
        assert_eq!(mac_status(3, 2), PermissionStatus::Limited);
        assert_eq!(mac_status(99, 2), PermissionStatus::Unknown);
    }
}
