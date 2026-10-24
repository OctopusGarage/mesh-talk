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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Serialize, Deserialize)]
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

/// Import complete records with one disk replacement, avoiding partial pin/alias state.
#[tauri::command]
pub async fn set_favorite_records(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, FavoritesState>,
    node: tauri::State<'_, crate::chat_commands::NodeState>,
    owner: String,
    expected_account: String,
    records: Vec<FavoriteInfo>,
    expected: HashMap<String, Option<FavoriteRecord>>,
) -> Result<(), CommandError> {
    let admission = crate::runtime_admission::RuntimeAdmission::capture(&app, &node, &owner)?;
    let path = config_store::account_path(&crate::data_dir(), &owner, FAVORITES_FILE)
        .ok_or_else(|| CommandError::Authorization("invalid account owner".into()))?;
    let state = state.inner().clone();
    // RuntimeAdmission retains installation and exact-session authority through
    // the blocking disk commit. Reentering config_store::with_account here would
    // acquire the same session lock a second time.
    admission
        .blocking(move |runtime, _| {
            replace_linked_records(
                &path,
                &state,
                runtime.account_id(),
                &expected_account,
                records,
                &expected,
            )
        })
        .await
}

fn replace_linked_records(
    path: &std::path::Path,
    state: &FavoritesState,
    current_account: &str,
    expected_account: &str,
    records: Vec<FavoriteInfo>,
    expected: &HashMap<String, Option<FavoriteRecord>>,
) -> Result<(), CommandError> {
    if current_account != expected_account {
        return Err(CommandError::Authorization(
            "Contact preferences belong to another account".into(),
        ));
    }
    let _guard = state
        .0
        .lock()
        .map_err(|_| CommandError::Internal("Favorites lock unavailable".into()))?;
    let mut file: FavoritesFile = config_store::read_account(path);
    check_expected(&file, &records, expected)?;
    apply_records(&mut file, records)?;
    config_store::write_account(path, &file)
}

fn check_expected(
    file: &FavoritesFile,
    records: &[FavoriteInfo],
    expected: &HashMap<String, Option<FavoriteRecord>>,
) -> Result<(), CommandError> {
    if records.iter().any(|row| {
        !expected.contains_key(&row.id)
            || file.contacts.get(&row.id)
                != expected.get(&row.id).and_then(|record| record.as_ref())
    }) {
        return Err(CommandError::Internal(
            "Local favorites changed during linked import".into(),
        ));
    }
    Ok(())
}

fn apply_records(file: &mut FavoritesFile, records: Vec<FavoriteInfo>) -> Result<(), CommandError> {
    if records.len() > 2048
        || records.iter().any(|record| {
            !matches!(record.id.len(), 32 | 64)
                || !record.id.bytes().all(|byte| byte.is_ascii_hexdigit())
                || record
                    .custom_alias
                    .as_ref()
                    .is_some_and(|alias| alias.len() > 1024 || alias.chars().count() > 256)
        })
    {
        return Err(CommandError::InvalidInput(
            "invalid favorite records".into(),
        ));
    }
    for record in records {
        let alias = record
            .custom_alias
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if !record.pinned && alias.is_none() && !record.hidden {
            file.contacts.remove(&record.id);
        } else {
            file.contacts.insert(
                record.id,
                FavoriteRecord {
                    pinned: record.pinned,
                    custom_alias: alias,
                    hidden: record.hidden,
                },
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    #[test]
    fn stale_crypto_account_cannot_replace_the_same_local_users_saved_alias() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("favorites.json");
        let state = FavoritesState::default();
        let id = "b".repeat(32);
        let original = FavoriteRecord {
            pinned: false,
            custom_alias: Some("Ronaldo".into()),
            hidden: false,
        };
        let file = FavoritesFile {
            contacts: HashMap::from([(id.clone(), original.clone())]),
        };
        config_store::write_account(&path, &file).unwrap();
        let before = std::fs::read(&path).unwrap();
        let records = vec![FavoriteInfo {
            id: id.clone(),
            pinned: false,
            custom_alias: Some("Previous account alias".into()),
            hidden: false,
        }];
        let expected = HashMap::from([(id, Some(original))]);
        assert!(
            replace_linked_records(
                &path,
                &state,
                "new-account",
                "old-account",
                records,
                &expected
            )
            .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn invalid_batch_does_not_apply_any_partial_favorite() {
        let mut file = FavoritesFile::default();
        let row = |id: &str| FavoriteInfo {
            id: id.into(),
            pinned: true,
            custom_alias: Some("Alice".into()),
            hidden: false,
        };
        assert!(apply_records(&mut file, vec![row(&"a".repeat(32)), row("invalid")]).is_err());
        assert!(file.contacts.is_empty());
        apply_records(&mut file, vec![row(&"a".repeat(32))]).unwrap();
        assert_eq!(
            file.contacts
                .values()
                .next()
                .unwrap()
                .custom_alias
                .as_deref(),
            Some("Alice")
        );
    }
    #[test]
    fn linked_batch_preserves_a_newer_local_favorite() {
        let id = "a".repeat(32);
        let mut file = FavoritesFile::default();
        apply_records(
            &mut file,
            vec![FavoriteInfo {
                id: id.clone(),
                pinned: true,
                custom_alias: Some("Local".into()),
                hidden: false,
            }],
        )
        .unwrap();
        let remote = vec![FavoriteInfo {
            id: id.clone(),
            pinned: false,
            custom_alias: Some("Remote".into()),
            hidden: false,
        }];
        let expected = HashMap::from([(id.clone(), None)]);
        assert!(check_expected(&file, &remote, &expected).is_err());
        assert_eq!(file.contacts[&id].custom_alias.as_deref(), Some("Local"));
    }
}
