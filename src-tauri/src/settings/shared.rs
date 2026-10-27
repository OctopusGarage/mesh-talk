//! Portable account choices. This allowlist deliberately excludes installation controls,
//! credentials, OS grants and physical paths; the account preference journal owns ordering.
use super::{AppSettings, CommandError};
use mesh_talk_core::node::AccountPreference;
use serde_json::Value;
use std::collections::BTreeMap;

const FIELDS: &[&str] = &[
    "notifications",
    "calls_enabled",
    "post_office_enabled",
    "post_office_send_enabled",
    "post_office_receive_enabled",
    "physical_lan_only",
    "bridge_discovery_enabled",
    "bridge_hub_id",
    "ringtone",
    "privacy_level",
    "notification_preview",
    "auto_lock_seconds",
    "lock_when_hidden",
    "confirm_sensitive_copy",
    "clipboard_clear_seconds",
];

pub(super) fn values(settings: &AppSettings) -> BTreeMap<String, String> {
    let value = serde_json::to_value(settings).expect("settings serialize");
    FIELDS
        .iter()
        .map(|field| (format!("settings.{field}"), value[*field].to_string()))
        .collect()
}

pub(super) fn changed(previous: &AppSettings, next: &AppSettings) -> BTreeMap<String, String> {
    let previous = values(previous);
    values(next)
        .into_iter()
        .filter(|(key, value)| previous.get(key) != Some(value))
        .collect()
}

fn valid(field: &str, value: &Value) -> bool {
    match field {
        "ringtone" => value
            .as_str()
            .is_some_and(|v| matches!(v, "classic" | "oldPhone" | "digital" | "marimba")),
        "privacy_level" => value
            .as_str()
            .is_some_and(|v| matches!(v, "balanced" | "private" | "strict" | "custom")),
        "bridge_hub_id" => value.as_str().is_some_and(|v| {
            v.is_empty() || (v.len() == 32 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        }),
        "auto_lock_seconds" | "clipboard_clear_seconds" => {
            value.as_u64().is_some_and(|v| u32::try_from(v).is_ok())
        }
        _ => value.is_boolean(),
    }
}

/// Tombstones reset only their own field; unknown/invalid future keys are ignored.
pub(super) fn project(settings: &AppSettings, records: &[AccountPreference]) -> AppSettings {
    let defaults = serde_json::to_value(AppSettings::default()).expect("settings serialize");
    let mut value = serde_json::to_value(settings).expect("settings serialize");
    for record in records {
        let Some(field) = record
            .key
            .strip_prefix("settings.")
            .filter(|field| FIELDS.contains(field))
        else {
            continue;
        };
        let proposed = match record.value.as_deref() {
            Some(text) => match serde_json::from_str::<Value>(text) {
                Ok(value) => value,
                Err(_) => continue,
            },
            None => defaults[field].clone(),
        };
        if valid(field, &proposed) {
            value[field] = proposed;
        }
    }
    serde_json::from_value(value).expect("validated portable settings")
}

/// Only explicit installation controls may be saved before the account runtime is ready.
pub(super) fn installation_patch(patch: &Value) -> bool {
    patch.as_object().is_some_and(|fields| {
        !fields.is_empty()
            && fields
                .keys()
                .all(|field| matches!(field.as_str(), "minimize_to_tray" | "stay_signed_in"))
    })
}

/// Apply a renderer patch to the latest acknowledged document inside host authority.
/// This prevents an intervening sibling update from being overwritten by a stale read.
pub(super) fn patch(settings: &AppSettings, patch: &Value) -> Result<AppSettings, CommandError> {
    let mut current = serde_json::to_value(settings).expect("settings serialize");
    let fields = patch
        .as_object()
        .ok_or_else(|| CommandError::InvalidInput("settings patch must be an object".into()))?;
    for (key, value) in fields {
        if key == "last_user" || !current.as_object().unwrap().contains_key(key) {
            return Err(CommandError::InvalidInput(
                "unknown or protected settings field".into(),
            ));
        }
        current[key] = value.clone();
    }
    serde_json::from_value(current).map_err(|error| CommandError::InvalidInput(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_talk_core::node::SyncCategory;
    fn record(key: &str, value: Option<&str>) -> AccountPreference {
        AccountPreference {
            category: SyncCategory::Appearance,
            key: format!("settings.{key}"),
            value: value.map(str::to_owned),
            revision: 1,
            device_id: "ab".repeat(16),
        }
    }
    #[test]
    fn startup_installation_patch_cannot_change_private_fields_or_credentials() {
        assert!(installation_patch(
            &serde_json::json!({"stay_signed_in": false, "minimize_to_tray": true})
        ));
        assert!(!installation_patch(
            &serde_json::json!({"notifications": false})
        ));
        assert!(!installation_patch(
            &serde_json::json!({"stay_signed_in": false, "retention_days": 1})
        ));
        assert!(!installation_patch(
            &serde_json::json!({"last_user": "other"})
        ));
        assert!(!installation_patch(&serde_json::json!({})));
    }
    #[test]
    fn local_retention_is_not_exported_as_an_account_preference() {
        let settings = AppSettings {
            retention_days: 90,
            ..AppSettings::default()
        };
        assert!(!values(&settings).contains_key("settings.retention_days"));
    }
    #[test]
    fn changing_only_local_retention_produces_no_shared_diff() {
        let previous = AppSettings {
            retention_days: 90,
            ..AppSettings::default()
        };
        let next = AppSettings {
            retention_days: 7,
            ..previous.clone()
        };
        assert!(changed(&previous, &next).is_empty());
    }
    #[test]
    fn legacy_remote_retention_values_and_tombstones_leave_local_retention_unchanged() {
        let original = AppSettings {
            retention_days: 90,
            ..AppSettings::default()
        };
        for remote in [Some("7"), Some("365"), Some("0"), None] {
            let next = project(
                &original,
                &[
                    record("retention_days", remote),
                    record("notifications", Some("false")),
                ],
            );
            assert_eq!(next.retention_days, 90, "remote retention: {remote:?}");
            assert!(!next.notifications);
        }
    }
    #[test]
    fn allowlist_never_exports_paths_credentials_or_installation_controls() {
        let settings = AppSettings {
            download_dir: "/private/files".into(),
            last_user: Some("secret-owner".into()),
            ..AppSettings::default()
        };
        let exported = values(&settings);
        assert_eq!(exported.len(), 15);
        for field in [
            "download_dir",
            "retention_days",
            "last_user",
            "stay_signed_in",
            "minimize_to_tray",
            "autostart",
            "access_token",
            "network_interface",
        ] {
            assert!(!exported.contains_key(&format!("settings.{field}")));
        }
    }
    #[test]
    fn projection_validates_types_and_resets_tombstones_without_touching_local_fields() {
        let original = AppSettings {
            download_dir: "/local".into(),
            notifications: false,
            ringtone: "marimba".into(),
            ..AppSettings::default()
        };
        let next = project(
            &original,
            &[
                record("notifications", None),
                record("ringtone", Some("\"digital\"")),
                record("download_dir", Some("\"/remote\"")),
                record("retention_days", Some("-1")),
                record("auto_lock_seconds", Some("4294967296")),
                record("calls_enabled", Some("\"true\"")),
                record("bridge_hub_id", Some("\"../invalid\"")),
            ],
        );
        assert!(next.notifications);
        assert_eq!(next.ringtone, "digital");
        assert_eq!(next.download_dir, "/local");
        assert_eq!(next.retention_days, 0);
        assert_eq!(next.auto_lock_seconds, 0);
        assert!(!next.calls_enabled);
        assert!(next.bridge_hub_id.is_empty());
        assert_eq!(changed(&original, &next).len(), 2);
    }
    #[test]
    fn renderer_patch_retains_concurrent_remote_fields_and_rejects_credentials() {
        let current = AppSettings {
            ringtone: "digital".into(),
            ..AppSettings::default()
        };
        let next = patch(&current, &serde_json::json!({"notifications": false})).unwrap();
        assert_eq!(next.ringtone, "digital");
        assert!(!next.notifications);
        assert!(patch(&current, &serde_json::json!({"last_user": "forged"})).is_err());
        assert!(patch(&current, &serde_json::json!({"notifications": "false"})).is_err());
    }
}
