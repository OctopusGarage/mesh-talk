//! JSON helpers for global app settings and account-scoped contact preferences.
//!
//! Contact files live under `accounts/<host owner>/`; legacy global contact files
//! cannot be assigned safely to an account and are archived without attribution.

use crate::{commands::CommandError, state::AppState};
use serde::{Serialize, de::DeserializeOwned};
use std::path::Path;
use std::path::PathBuf;

/// Files belonging to a signed-in host account live beside its node profile.
pub fn account_path(root: &Path, owner: &str, filename: &str) -> Option<PathBuf> {
    if owner.is_empty()
        || owner.len() > 128
        || !owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    Some(root.join("accounts").join(owner).join(filename))
}

/// Hold session authority through the whole local read or write. The explicit owner
/// prevents a queued command from being applied to a replacement login.
pub fn with_account<T>(
    app: &AppState,
    owner: &str,
    filename: &str,
    operation: impl FnOnce(&Path) -> Result<T, CommandError>,
) -> Result<T, CommandError> {
    app.session()
        .with_owner(|current| {
            if current != Some(owner) || app.app_lock().is_locked() {
                return Err("account session mismatch or app locked".into());
            }
            let path =
                account_path(&crate::data_dir(), owner, filename).ok_or("invalid account owner")?;
            Ok(operation(&path))
        })
        .map_err(CommandError::Authorization)?
}

/// Preserve the originating generation across asynchronous settings application.
pub(crate) fn with_account_lease<T>(
    app: &AppState,
    lease: &crate::state::SessionLease,
    filename: &str,
    operation: impl FnOnce(&Path) -> Result<T, CommandError>,
) -> Result<T, CommandError> {
    app.session()
        .matching(lease, |_| {
            if app.app_lock().is_locked() {
                return Err(CommandError::Authorization("app locked".into()));
            }
            let path = account_path(&crate::data_dir(), lease.owner(), filename)
                .ok_or_else(|| CommandError::Authorization("invalid account owner".into()))?;
            operation(&path)
        })
        .map_err(CommandError::Authorization)?
}

pub fn read_account<T: DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn write_account<T: Serialize>(path: &Path, value: &T) -> Result<(), CommandError> {
    prepare_account_write(path, value)?.commit()
}

/// Serialization and durable staging can run without session authority. Only
/// the atomic replacement needs the caller's exact-session commit guard.
pub(crate) struct PreparedAccountWrite {
    temporary: tempfile::NamedTempFile,
    destination: PathBuf,
}

impl PreparedAccountWrite {
    pub(crate) fn commit(self) -> Result<(), CommandError> {
        self.temporary
            .persist(self.destination)
            .map_err(|e| CommandError::Io(e.to_string()))?;
        Ok(())
    }
}

pub(crate) fn prepare_account_write<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<PreparedAccountWrite, CommandError> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| CommandError::Io("invalid account path".into()))?;
    std::fs::create_dir_all(parent).map_err(|e| CommandError::Io(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| CommandError::Io(e.to_string()))?;
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|e| CommandError::Io(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| CommandError::Io(e.to_string()))?;
    }
    serde_json::to_writer_pretty(&mut temporary, value)
        .map_err(|e| CommandError::Internal(e.to_string()))?;
    temporary
        .flush()
        .map_err(|e| CommandError::Io(e.to_string()))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|e| CommandError::Io(e.to_string()))?;
    Ok(PreparedAccountWrite {
        temporary,
        destination: path.to_owned(),
    })
}

/// `<app config dir>/<filename>`, or `None` if there is no app config dir.
pub fn config_path<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    filename: &str,
) -> Option<PathBuf> {
    use tauri::Manager;
    if let Some(root) = crate::configured_data_root() {
        return Some(root.join("config").join(filename));
    }
    app.path().app_config_dir().ok().map(|d| d.join(filename))
}

/// Move app-wide contact metadata aside before any account can read it. These
/// files contain no owner, so silently importing them into a login is unsafe.
pub fn quarantine_legacy<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let Some(path) = config_path(app, "favorites.json") else {
        return;
    };
    if let Some(root) = path.parent()
        && let Err(error) = quarantine_legacy_at(root)
    {
        log::warn!("Could not quarantine legacy contact preferences: {error}");
    }
}

fn quarantine_legacy_at(root: &Path) -> std::io::Result<()> {
    let archive = root.join("legacy-unassigned");
    for filename in ["favorites.json", "trust.json", "avatars.json"] {
        let source = root.join(filename);
        if !source.exists() {
            continue;
        }
        std::fs::create_dir_all(&archive)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&archive, std::fs::Permissions::from_mode(0o700))?;
            std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600))?;
        }
        let mut destination = archive.join(filename);
        for index in 1..=1000 {
            if !destination.exists() {
                break;
            }
            destination = archive.join(format!("{filename}.{index}"));
        }
        if destination.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "legacy archive full",
            ));
        }
        std::fs::rename(source, destination)?;
    }
    Ok(())
}

/// Preserve the old mixed global settings document before removing its unowned
/// account preferences. The archive is never loaded as an account's settings.
pub fn archive_legacy_settings<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> std::io::Result<()> {
    let Some(source) = config_path(app, "settings.json") else {
        return Ok(());
    };
    archive_legacy_settings_at(&source)
}

fn archive_legacy_settings_at(source: &Path) -> std::io::Result<()> {
    let raw = match std::fs::read(source) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let parsed: serde_json::Value = serde_json::from_slice(&raw)?;
    let Some(object) = parsed.as_object() else {
        return Ok(());
    };
    if !object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "minimize_to_tray" | "stay_signed_in" | "last_user"
        )
    }) {
        return Ok(());
    }
    let archive = source.parent().unwrap().join("legacy-unassigned");
    std::fs::create_dir_all(&archive)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&archive, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut destination = archive.join("settings.json");
    for index in 1..=1000 {
        if !destination.exists() {
            break;
        }
        destination = archive.join(format!("settings.json.{index}"));
    }
    if destination.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "legacy archive full",
        ));
    }
    std::fs::write(&destination, raw)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Load a JSON config value, falling back to `T::default()` when the file is
/// missing, unreadable, or corrupt (the missing/corrupt case is silent so a single
/// bad field doesn't wipe everything on the next save — callers' per-field
/// `#[serde(default)]` handles forward-compat within a parseable file).
pub fn load<R: tauri::Runtime, T: DeserializeOwned + Default>(
    app: &tauri::AppHandle<R>,
    filename: &str,
) -> T {
    config_path(app, filename)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<T>(&s).ok())
        .unwrap_or_default()
}

/// Persist a JSON config value (pretty-printed) to `<app config dir>/<filename>`.
/// Best-effort: every failure is logged with `what` for context and otherwise ignored.
pub fn save<R: tauri::Runtime, T: Serialize>(
    app: &tauri::AppHandle<R>,
    filename: &str,
    what: &str,
    value: &T,
) {
    let Some(path) = config_path(app, filename) else {
        log::warn!("No app config dir; {what} not persisted");
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(value) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                log::warn!("Failed to persist {what}: {e}");
            }
        }
        Err(e) => log::warn!("Failed to serialize {what}: {e}"),
    }
}

#[cfg(test)]
mod account_tests {
    use super::*;
    use crate::services::{auth_service::AuthService, user::User};
    use mesh_talk_core::{identity::manager::IdentityManager, storage::file_manager::FileManager};
    use std::sync::Arc;

    #[test]
    fn prepared_account_write_changes_nothing_until_commit_and_cleans_up_on_drop() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("favorites.json");
        write_account(&path, &serde_json::json!({"old": true})).unwrap();
        let staged = prepare_account_write(&path, &serde_json::json!({"new": true})).unwrap();
        assert_eq!(
            read_account::<serde_json::Value>(&path),
            serde_json::json!({"old": true})
        );
        drop(staged);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        let staged = prepare_account_write(&path, &serde_json::json!({"new": true})).unwrap();
        staged.commit().unwrap();
        assert_eq!(
            read_account::<serde_json::Value>(&path),
            serde_json::json!({"new": true})
        );
    }

    #[test]
    fn contact_preferences_are_isolated_by_host_account() {
        let root = tempfile::tempdir().unwrap();
        let alice = account_path(root.path(), "alice", "favorites.json").unwrap();
        let bob = account_path(root.path(), "bob", "favorites.json").unwrap();
        assert_ne!(alice, bob);
        assert_eq!(alice, root.path().join("accounts/alice/favorites.json"));
        assert_eq!(bob, root.path().join("accounts/bob/favorites.json"));
        assert!(account_path(root.path(), "../alice", "trust.json").is_none());
        assert!(account_path(root.path(), "alice/bob", "avatars.json").is_none());
        write_account(&alice, &serde_json::json!({"contact": "Alice's alias"})).unwrap();
        write_account(&bob, &serde_json::json!({"contact": "Bob's alias"})).unwrap();
        let alice_data: serde_json::Value = read_account(&alice);
        let bob_data: serde_json::Value = read_account(&bob);
        assert_eq!(alice_data["contact"], "Alice's alias");
        assert_eq!(bob_data["contact"], "Bob's alias");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(alice.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&alice).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn queued_contact_operation_cannot_follow_a_replacement_login() {
        let root = tempfile::tempdir().unwrap();
        let app = AppState::new(AuthService::new(Arc::new(IdentityManager::new(
            FileManager::new(root.path().to_owned()),
        ))));
        let user = |owner: &str| User {
            user_id: owner.into(),
            account_login_name: None,
            name: owner.into(),
            display_name: owner.into(),
            address: String::new(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        };
        app.session()
            .set("token".into(), user("alice"), "password".into());
        assert!(with_account(&app, "alice", "trust.json", |_| Ok(())).is_ok());
        app.session()
            .set("token".into(), user("bob"), "password".into());
        assert!(with_account(&app, "alice", "trust.json", |_| Ok(())).is_err());
        assert!(with_account(&app, "bob", "trust.json", |_| Ok(())).is_ok());
    }

    #[test]
    fn unowned_legacy_contact_files_leave_the_global_lookup_location() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("trust.json"), b"private").unwrap();
        quarantine_legacy_at(root.path()).unwrap();
        assert!(!root.path().join("trust.json").exists());
        assert_eq!(
            std::fs::read(root.path().join("legacy-unassigned/trust.json")).unwrap(),
            b"private"
        );
        std::fs::write(root.path().join("trust.json"), b"second").unwrap();
        quarantine_legacy_at(root.path()).unwrap();
        assert_eq!(
            std::fs::read(root.path().join("legacy-unassigned/trust.json.1")).unwrap(),
            b"second"
        );
    }

    #[test]
    fn unowned_legacy_settings_are_preserved_before_global_rewrite() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("settings.json");
        let old = br#"{"minimize_to_tray":true,"download_dir":"/alice/private"}"#;
        std::fs::write(&source, old).unwrap();
        archive_legacy_settings_at(&source).unwrap();
        assert_eq!(
            std::fs::read(root.path().join("legacy-unassigned/settings.json")).unwrap(),
            old
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.path().join("legacy-unassigned/settings.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
