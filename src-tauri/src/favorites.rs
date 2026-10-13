//! Favorites / pinned contacts: per-contact UI preferences the user sets locally —
//! whether a contact is pinned to the top of the sidebar, and an optional custom alias
//! that overrides the announced name. This is purely local UI state (no crypto, no
//! protocol), persisted as JSON in the signed-in host account's profile.
//!
//! Keyed by the stable contact identity used elsewhere in the UI: the `account_id`
//! (a multi-device contact is one entry; channels use their `channel_id`). The store
//! is opaque to what the key means — it just maps an id to a record.

use crate::commands::CommandError;
use crate::config_store;
use crate::state::AppState;
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
    /// Local-only channel visibility; does not change membership or delete history.
    #[serde(default)]
    pub hidden: bool,
}

/// The on-disk shape: `id` (account_id / channel_id) -> record.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct FavoritesFile {
    #[serde(default)]
    contacts: HashMap<String, FavoriteRecord>,
}

/// Serializes read/modify/write operations within the process.
#[derive(Clone, Default)]
pub struct FavoritesState(Arc<Mutex<()>>);

/// One favorites entry returned to the UI.
#[derive(Clone, Debug, Serialize)]
pub struct FavoriteInfo {
    pub id: String,
    pub pinned: bool,
    pub custom_alias: Option<String>,
    pub hidden: bool,
}

/// The persistence file inside the host account's profile.
const FAVORITES_FILE: &str = "favorites.json";

/// Snapshot every favorites entry the user has set (pin and/or alias).
#[tauri::command]
pub fn get_favorites(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, FavoritesState>,
    owner: String,
) -> Result<Vec<FavoriteInfo>, CommandError> {
    config_store::with_account(&app, &owner, FAVORITES_FILE, |path| {
        let _guard = state.0.lock().unwrap();
        let file: FavoritesFile = config_store::read_account(path);
        Ok(file
            .contacts
            .iter()
            .map(|(id, rec)| FavoriteInfo {
                id: id.clone(),
                pinned: rec.pinned,
                custom_alias: rec.custom_alias.clone(),
                hidden: rec.hidden,
            })
            .collect())
    })
}

/// Pin or unpin a contact. Prunes the entry once it carries no preferences (unpinned
/// with no alias) so the file stays a record of only what the user actually set.
#[tauri::command]
pub fn set_favorite(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, FavoritesState>,
    owner: String,
    id: String,
    pinned: bool,
) -> Result<(), CommandError> {
    config_store::with_account(&app, &owner, FAVORITES_FILE, |path| {
        let _guard = state.0.lock().unwrap();
        let mut file: FavoritesFile = config_store::read_account(path);
        let rec = file.contacts.entry(id.clone()).or_default();
        rec.pinned = pinned;
        if !rec.pinned && rec.custom_alias.is_none() && !rec.hidden {
            file.contacts.remove(&id);
        }
        config_store::write_account(path, &file)
    })
}

/// Set or clear a contact's custom alias. A blank/whitespace alias clears it (and prunes
/// the entry if nothing else is set), so renaming back to empty restores the announced name.
#[tauri::command]
pub fn set_alias(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, FavoritesState>,
    owner: String,
    id: String,
    alias: Option<String>,
) -> Result<(), CommandError> {
    let trimmed = alias
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty());
    config_store::with_account(&app, &owner, FAVORITES_FILE, |path| {
        let _guard = state.0.lock().unwrap();
        let mut file: FavoritesFile = config_store::read_account(path);
        let rec = file.contacts.entry(id.clone()).or_default();
        rec.custom_alias = trimmed;
        if !rec.pinned && rec.custom_alias.is_none() && !rec.hidden {
            file.contacts.remove(&id);
        }
        config_store::write_account(path, &file)
    })
}

/// Hide or restore a channel on this device. The channel remains a member and
/// continues to sync; this only changes the local conversation list.
#[tauri::command]
pub fn set_channel_hidden(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, FavoritesState>,
    owner: String,
    id: String,
    hidden: bool,
) -> Result<(), CommandError> {
    config_store::with_account(&app, &owner, FAVORITES_FILE, |path| {
        let _guard = state.0.lock().unwrap();
        let mut file: FavoritesFile = config_store::read_account(path);
        let rec = file.contacts.entry(id.clone()).or_default();
        rec.hidden = hidden;
        if !rec.pinned && rec.custom_alias.is_none() && !rec.hidden {
            file.contacts.remove(&id);
        }
        config_store::write_account(path, &file)
    })
}
