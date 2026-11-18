//! Machine notification authorization. The desktop plugin's permission API is
//! an unconditional grant; it is not evidence of current OS authorization.
use crate::commands::CommandError;
use serde::Serialize;
use tauri_plugin_opener::OpenerExt;

#[cfg(target_os = "macos")]
mod mac_callbacks;

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
pub async fn notification_permission<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> NotificationPermission {
    observe(app, false).await
}

#[tauri::command]
pub async fn request_notification_permission<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> NotificationPermission {
    observe(app, true).await
}

async fn observe<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    request: bool,
) -> NotificationPermission {
    let status = tauri::async_runtime::spawn_blocking(move || platform_permission(&app, request))
        .await
        .unwrap_or(PermissionStatus::Unknown);
    snapshot(status)
}

/// Only fixed OS destinations are accepted; opening them is an explicit UI action.
#[tauri::command]
pub async fn open_notification_settings<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<(), CommandError> {
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
        // macOS can retain enabled legacy presentation settings while reporting
        // undetermined/denied modern authorization. Neither grant nor denial is
        // established by that contradictory snapshot; do not nag the user.
        0 | 1 if alerts == 2 => PermissionStatus::Unknown,
        0 => PermissionStatus::Prompt,
        1 => PermissionStatus::Denied,
        2 | 4 if alerts == 2 => PermissionStatus::Granted,
        2..=4 => PermissionStatus::Limited,
        _ => PermissionStatus::Unknown,
    }
}

/// Deliver through the same native identity used by permission observation.
/// Submission is best effort; callbacks never retain message text or session state.
pub(crate) fn send<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    title: &str,
    body: &str,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let center = mac_center(app).ok_or("Notification application identity unavailable")?;
        let request = mac_request(title, body);
        let reply = mac_callbacks::submission();
        center.addNotificationRequest_withCompletionHandler(&request, Some(&reply));
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        use tauri_plugin_notification::NotificationExt;
        app.notification()
            .builder()
            .title(title)
            .body(body)
            .show()
            .map_err(|error| error.to_string())
    }
}

#[cfg(target_os = "macos")]
fn mac_request(
    title: &str,
    body: &str,
) -> objc2::rc::Retained<objc2_user_notifications::UNNotificationRequest> {
    use objc2_foundation::NSString;
    use objc2_user_notifications::{UNMutableNotificationContent, UNNotificationRequest};
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    let identifier = NSString::from_str(&format!("{:032x}", rand::random::<u128>()));
    UNNotificationRequest::requestWithIdentifier_content_trigger(&identifier, &content, None)
}

#[cfg(target_os = "macos")]
fn mac_center<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Option<objc2::rc::Retained<objc2_user_notifications::UNUserNotificationCenter>> {
    // A bare executable can raise an Objective-C exception. Never substitute
    // Terminal's identity, for either delivery or permission observation.
    let bundle = objc2_foundation::NSBundle::mainBundle();
    if tauri::is_dev()
        || bundle
            .bundleIdentifier()
            .is_none_or(|id| id.to_string() != app.config().identifier)
    {
        return None;
    }
    Some(objc2_user_notifications::UNUserNotificationCenter::currentNotificationCenter())
}

#[cfg(all(test, target_os = "macos"))]
fn assert_native_prompt_allowed(role: Option<&str>) {
    assert_ne!(
        role,
        Some("unconfigured"),
        "the unconfigured CI control cannot request authorization"
    );
}

#[cfg(target_os = "macos")]
fn platform_permission<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    request: bool,
) -> PermissionStatus {
    use block2::RcBlock;
    use objc2_user_notifications::{UNAuthorizationOptions, UNNotificationSettings};
    use std::{ptr::NonNull, sync::mpsc, time::Duration};

    let Some(center) = mac_center(app) else {
        return PermissionStatus::Unknown;
    };
    let read = || {
        let (sender, receiver) = mpsc::sync_channel(1);
        let reply = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            // Apple's callback owns the settings for the duration of this call.
            let settings = unsafe { settings.as_ref() };
            #[cfg(test)]
            eprintln!(
                "native notification settings: authorization={}, alerts={}, center={}",
                settings.authorizationStatus().0,
                settings.alertSetting().0,
                settings.notificationCenterSetting().0
            );
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
    let reply = mac_callbacks::authorization(sender);
    // The hosted coverage control must fail before any OS prompt, even if its
    // caller accidentally regresses the observation-only admission above.
    #[cfg(test)]
    assert_native_prompt_allowed(
        std::env::var("MESH_NOTIFICATION_NATIVE_EXPECT")
            .ok()
            .as_deref(),
    );
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
fn platform_permission<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _request: bool,
) -> PermissionStatus {
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
fn platform_permission<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    _request: bool,
) -> PermissionStatus {
    // Linux notification daemons have no portable per-app permission query or
    // settings URI. A successful DBus connection cannot prove alerts are enabled.
    PermissionStatus::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run only from a bundled test executable with this isolated identifier.
    /// MESH_NOTIFICATION_NATIVE_EXPECT names the expected status; REQUEST=1 is
    /// an explicit operator action, never part of ordinary tests or startup.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires isolated macOS bundle and operator-controlled system permission"]
    fn mac_packaged_permission_and_delivery() {
        use block2::RcBlock;
        use objc2_foundation::NSArray;
        use objc2_user_notifications::UNNotification;
        use std::{
            ptr::NonNull,
            sync::mpsc,
            time::{Duration, Instant},
        };
        let expected =
            std::env::var("MESH_NOTIFICATION_NATIVE_EXPECT").expect("explicit expected permission");
        let delivery = std::env::var("MESH_NOTIFICATION_NATIVE_DELIVERY")
            .expect("explicit delivery expectation");
        assert!(matches!(
            delivery.as_str(),
            "present" | "absent" | "reference"
        ));
        let compare_reference = delivery == "reference";
        assert!(
            !compare_reference || expected == "unconfigured",
            "reference comparison belongs to the unconfigured hosted control"
        );
        let expect_delivery = delivery == "present";
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().identifier = "com.mesh-talk.notification-validation".into();
        let app = tauri::test::mock_builder().build(context).unwrap();
        let center = mac_center(app.handle()).expect("run the bundled test executable");
        let request = std::env::var("MESH_NOTIFICATION_NATIVE_REQUEST").as_deref() == Ok("1");
        if expected == "unconfigured" {
            assert!(!request, "CI must never request notification authorization");
        }
        let status = platform_permission(app.handle(), request);
        let raw_settings = || {
            let (tx, rx) = mpsc::sync_channel(1);
            let reply = RcBlock::new(
                move |settings: NonNull<objc2_user_notifications::UNNotificationSettings>| {
                    let settings = unsafe { settings.as_ref() };
                    let _ = tx.send((
                        settings.authorizationStatus().0,
                        settings.alertSetting().0,
                        settings.notificationCenterSetting().0,
                    ));
                },
            );
            center.getNotificationSettingsWithCompletionHandler(&reply);
            rx.recv_timeout(Duration::from_secs(2)).unwrap()
        };
        let require_unconfigured = |status| {
            let (authorization, alerts, notification_center) = raw_settings();
            eprintln!(
                "unconfigured native control: authorization={authorization}, alerts={alerts}, center={notification_center}"
            );
            assert_eq!(authorization, 0, "CI requires unconfigured authorization");
            assert_eq!(
                status,
                if alerts == 2 {
                    PermissionStatus::Unknown
                } else {
                    PermissionStatus::Prompt
                }
            );
        };
        if expected == "unconfigured" {
            require_unconfigured(status);
        } else {
            assert_eq!(serde_json::to_value(status).unwrap(), expected);
        }
        let title = format!(
            "Mesh-Talk notification validation {:032x}",
            rand::random::<u128>()
        );
        let reference_title = format!("Mesh-Talk native reference {:032x}", rand::random::<u128>());
        let deadline = Instant::now() + Duration::from_secs(8);
        if compare_reference {
            use objc2_foundation::{NSError, NSString};
            use objc2_user_notifications::{UNMutableNotificationContent, UNNotificationRequest};
            // Build the oracle independently: using mac_request/send here would
            // let a broken product request fail identically and pass comparison.
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(&reference_title));
            content.setBody(&NSString::from_str("Native reference delivery check"));
            let identifier =
                NSString::from_str(&format!("reference-{:032x}", rand::random::<u128>()));
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
                &identifier,
                &content,
                None,
            );
            let (tx, rx) = mpsc::sync_channel(1);
            let completion = RcBlock::new(move |error: *mut NSError| {
                let _ = tx.send(unsafe { error.as_ref() }.map(|error| error.code()));
            });
            center.addNotificationRequest_withCompletionHandler(&request, Some(&completion));
            let error = rx
                .recv_timeout(Duration::from_secs(2))
                .expect("native reference submission completion");
            eprintln!("native reference submission error code: {error:?}");
        }
        send(app.handle(), &title, "Native notification delivery check").unwrap();
        loop {
            let (tx, rx) = mpsc::sync_channel(1);
            let title = title.clone();
            let reference_title = reference_title.clone();
            let reply = RcBlock::new(move |items: NonNull<NSArray<UNNotification>>| {
                let (mut product, mut reference, mut wrong_body) = (false, false, false);
                for item in unsafe { items.as_ref() }.iter() {
                    let content = item.request().content();
                    let observed_title = content.title().to_string();
                    if observed_title == title {
                        product = true;
                        wrong_body |=
                            content.body().to_string() != "Native notification delivery check";
                    }
                    if observed_title == reference_title {
                        reference = true;
                        wrong_body |=
                            content.body().to_string() != "Native reference delivery check";
                    }
                }
                let _ = tx.send((product, reference, wrong_body));
            });
            center.getDeliveredNotificationsWithCompletionHandler(&reply);
            let (found, reference_found, wrong_body) = rx
                .recv_timeout(Duration::from_secs(2))
                .expect("native delivery observation");
            assert!(
                !wrong_body,
                "native delivery must preserve the exact payload"
            );
            let expired = Instant::now() >= deadline;
            if compare_reference {
                if reference_delivery_complete(found, reference_found, expired)
                    .expect("product delivery differs from the independent native reference")
                {
                    eprintln!(
                        "native delivery comparison: product={found}, reference={reference_found}"
                    );
                    break;
                }
            } else {
                if expect_delivery && found {
                    break;
                }
                assert!(
                    !found,
                    "notification unexpectedly delivered: before={status:?}, after={:?}",
                    platform_permission(app.handle(), false)
                );
                if expired {
                    assert!(
                        !expect_delivery,
                        "notification never reached Notification Center"
                    );
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if expected == "unconfigured" {
            require_unconfigured(platform_permission(app.handle(), false));
        }
        // The center is bound to the dedicated fixture, never the installed app.
        center.removeAllDeliveredNotifications();
    }

    #[cfg(target_os = "macos")]
    fn reference_delivery_complete(
        product: bool,
        reference: bool,
        expired: bool,
    ) -> Result<bool, &'static str> {
        if product && reference {
            return Ok(true);
        }
        if !expired {
            return Ok(false);
        }
        if product == reference {
            Ok(true)
        } else {
            Err("asymmetric native delivery")
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn hosted_delivery_reference_requires_actual_agreement_and_the_absence_deadline() {
        assert_eq!(reference_delivery_complete(false, false, false), Ok(false));
        assert_eq!(reference_delivery_complete(true, false, false), Ok(false));
        assert_eq!(reference_delivery_complete(false, true, false), Ok(false));
        assert_eq!(reference_delivery_complete(true, true, false), Ok(true));
        assert_eq!(reference_delivery_complete(false, false, true), Ok(true));
        assert!(
            reference_delivery_complete(false, true, true).is_err(),
            "a product no-op cannot pass when native delivery succeeds"
        );
        assert!(
            reference_delivery_complete(true, false, true).is_err(),
            "extra product delivery cannot pass against native absence"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_hosted_control_refuses_prompting_before_the_native_api() {
        assert!(
            std::panic::catch_unwind(|| super::assert_native_prompt_allowed(Some("unconfigured")))
                .is_err()
        );
        super::assert_native_prompt_allowed(None);
        super::assert_native_prompt_allowed(Some("granted"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_delivery_rejects_the_same_unavailable_identity_as_observation() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        assert_eq!(
            platform_permission(app.handle(), false),
            PermissionStatus::Unknown
        );
        assert!(send(app.handle(), "Sender", "Message").is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_delivery_preserves_preview_and_uses_distinct_request_identifiers() {
        let first = mac_request("Sender", "Message 🔒");
        let second = mac_request("Sender", "Message 🔒");
        assert_eq!(first.content().title().to_string(), "Sender");
        assert_eq!(first.content().body().to_string(), "Message 🔒");
        assert_ne!(first.identifier(), second.identifier());
        assert!(first.trigger().is_none());
    }

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
        assert_eq!(mac_status(0, 2), PermissionStatus::Unknown);
        assert_eq!(mac_status(1, 2), PermissionStatus::Unknown);
        assert_eq!(mac_status(1, 1), PermissionStatus::Denied);
        assert_eq!(mac_status(2, 2), PermissionStatus::Granted);
        assert_eq!(mac_status(2, 1), PermissionStatus::Limited);
        assert_eq!(mac_status(3, 2), PermissionStatus::Limited);
        assert_eq!(mac_status(99, 2), PermissionStatus::Unknown);
    }

    #[test]
    fn registered_permission_commands_report_unavailable_identity_without_prompting() {
        let app = tauri::test::mock_builder()
            .invoke_handler(tauri::generate_handler![
                notification_permission,
                request_notification_permission,
                open_notification_settings
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let invoke = |command: &str| {
            tauri::test::get_ipc_response(
                &webview,
                tauri::webview::InvokeRequest {
                    cmd: command.into(),
                    callback: tauri::ipc::CallbackFn(0),
                    error: tauri::ipc::CallbackFn(1),
                    url: if cfg!(windows) {
                        "http://tauri.localhost"
                    } else {
                        "tauri://localhost"
                    }
                    .parse()
                    .unwrap(),
                    body: tauri::ipc::InvokeBody::Json(serde_json::json!({})),
                    headers: Default::default(),
                    invoke_key: tauri::test::INVOKE_KEY.into(),
                },
            )
            .map(|body| body.deserialize::<serde_json::Value>().unwrap())
        };
        for command in ["notification_permission", "request_notification_permission"] {
            let value = invoke(command).unwrap();
            assert_eq!(
                value["status"], "unknown",
                "a headless/uninstalled host must never claim a grant"
            );
            assert_eq!(
                value,
                serde_json::to_value(snapshot(PermissionStatus::Unknown)).unwrap()
            );
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let error = invoke("open_notification_settings").unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("Open your desktop notification settings manually")
            );
        }
    }
}
