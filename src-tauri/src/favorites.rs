//! Favorites / pinned contacts: per-contact UI preferences the user sets locally —
//! whether a contact is pinned to the top of the sidebar, and an optional custom alias
//! that overrides the announced name. This is purely local UI state (no crypto, no
//! protocol), persisted as JSON in the app config dir like `settings.rs` / `trust.rs`.
//!
//! Keyed by the stable contact identity used elsewhere in the UI: the `account_id`
//! (a multi-device contact is one entry; channels use their `channel_id`). The store
//! is opaque to what the key means — it just maps an id to a record.

use crate::commands::CommandError;
use crate::config_store;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// What we persist about one contact's local UI preferences.
///
/// Self-describing JSON-at-rest, so per-field `#[serde(default)]` is the forward-compat
/// tool: a file written by an older build still loads (missing fields fall back to
/// their defaults) instead of failing the whole parse.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FavoriteRecord {
    /// Pinned contacts sort to the top of the sidebar.
    #[serde(default)]
    pub pinned: bool,
    /// A user-set alias that overrides the announced name in the UI. `None` = use the
    /// announced name.
    #[serde(default)]
    pub custom_alias: Option<String>,
}

/// The on-disk shape: `id` (account_id / channel_id) -> record.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct FavoritesFile {
    #[serde(default)]
    contacts: HashMap<String, FavoriteRecord>,
}

/// Managed Tauri state: the in-memory favorites table behind a `Mutex`, seeded from disk
/// on setup and written through on every change.
#[derive(Clone, Default)]
pub struct FavoritesState(Arc<Mutex<FavoritesFile>>);

/// One favorites entry returned to the UI.
#[derive(Clone, Debug, Serialize)]
pub struct FavoriteInfo {
    pub id: String,
    pub pinned: bool,
    pub custom_alias: Option<String>,
}

/// The persistence file for the favorites table, under the app config dir.
const FAVORITES_FILE: &str = "favorites.json";

/// Load the persisted favorites table (empty if missing/unreadable) into managed state.
pub fn load_into_state<R: tauri::Runtime>(app: &tauri::AppHandle<R>, state: &FavoritesState) {
    *state.0.lock().unwrap() = config_store::load::<R, FavoritesFile>(app, FAVORITES_FILE);
}

/// Persist the favorites table to disk (best-effort; logged on failure).
fn save<R: tauri::Runtime>(app: &tauri::AppHandle<R>, file: &FavoritesFile) {
    config_store::save(app, FAVORITES_FILE, "favorites", file);
}

/// Snapshot every favorites entry the user has set (pin and/or alias).
#[tauri::command]
pub fn get_favorites(state: tauri::State<'_, FavoritesState>) -> Vec<FavoriteInfo> {
    let file = state.0.lock().unwrap();
    file.contacts
        .iter()
        .map(|(id, rec)| FavoriteInfo {
            id: id.clone(),
            pinned: rec.pinned,
            custom_alias: rec.custom_alias.clone(),
        })
        .collect()
}

/// Pin or unpin a contact. Prunes the entry once it carries no preferences (unpinned
/// with no alias) so the file stays a record of only what the user actually set.
#[tauri::command]
pub fn set_favorite(
    app: tauri::AppHandle,
    state: tauri::State<'_, FavoritesState>,
    id: String,
    pinned: bool,
) -> Result<(), CommandError> {
    let snapshot = {
        let mut file = state.0.lock().unwrap();
        let rec = file.contacts.entry(id.clone()).or_default();
        rec.pinned = pinned;
        if !rec.pinned && rec.custom_alias.is_none() {
            file.contacts.remove(&id);
        }
        file.clone()
    };
    save(&app, &snapshot);
    Ok(())
}

/// Set or clear a contact's custom alias. A blank/whitespace alias clears it (and prunes
/// the entry if nothing else is set), so renaming back to empty restores the announced name.
#[tauri::command]
pub fn set_alias(
    app: tauri::AppHandle,
    state: tauri::State<'_, FavoritesState>,
    id: String,
    alias: Option<String>,
) -> Result<(), CommandError> {
    let trimmed = alias
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty());
    let snapshot = {
        let mut file = state.0.lock().unwrap();
        let rec = file.contacts.entry(id.clone()).or_default();
        rec.custom_alias = trimmed;
        if !rec.pinned && rec.custom_alias.is_none() {
            file.contacts.remove(&id);
        }
        file.clone()
    };
    save(&app, &snapshot);
    Ok(())
}
