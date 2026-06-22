//! Shared JSON config-store helpers for the small per-feature preference files that
//! live in the app config dir (`settings.json`, `trust.json`, `favorites.json`).
//!
//! Each of those modules keeps its OWN typed state wrapper, its commands, and its
//! per-struct `#[serde(default)]` forward-compat. This module only collapses the
//! identical path / load / save file-IO scaffolding into generics over `T`.

use serde::{de::DeserializeOwned, Serialize};
use std::path::PathBuf;

/// `<app config dir>/<filename>`, or `None` if there is no app config dir.
pub fn config_path<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    filename: &str,
) -> Option<PathBuf> {
    use tauri::Manager;
    app.path().app_config_dir().ok().map(|d| d.join(filename))
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
