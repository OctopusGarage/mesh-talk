//! Custom avatars: per-identity profile photos the user sets locally. A user can attach
//! a custom image for THEMSELVES or for any CONTACT, replacing the deterministic
//! `IdentityGlyph` wherever that identity renders. This is purely local UI state (no
//! crypto, no protocol), persisted as JSON in the app config dir like `favorites.rs` /
//! `trust.rs`.
//!
//! NOTE — out of scope: this never propagates your avatar to peers over the network.
//! Doing so would need a profile-exchange protocol (a signed profile blob distributed
//! over the mesh); that is tracked as a follow-up.
//!
//! Keyed by the same stable identity the UI uses elsewhere (the `account_id` for a
//! contact / yourself, a `channel_id` for a channel). The value is a data-URL string
//! (`data:image/jpeg;base64,…` / PNG). The frontend resizes images before sending, so
//! entries stay small (~10–30KB).

use crate::commands::CommandError;
use crate::config_store;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The on-disk shape: `id` (account_id / channel_id) -> data-URL string.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct AvatarsFile {
    #[serde(default)]
    avatars: HashMap<String, String>,
}

/// Managed Tauri state: the in-memory avatar table behind a `Mutex`, seeded from disk on
/// setup and written through on every change.
#[derive(Clone, Default)]
pub struct AvatarsState(Arc<Mutex<AvatarsFile>>);

/// The persistence file for the avatar table, under the app config dir.
const AVATARS_FILE: &str = "avatars.json";

/// Load the persisted avatar table (empty if missing/unreadable) into managed state.
pub fn load_into_state<R: tauri::Runtime>(app: &tauri::AppHandle<R>, state: &AvatarsState) {
    *state.0.lock().unwrap() = config_store::load::<R, AvatarsFile>(app, AVATARS_FILE);
}

/// Persist the avatar table to disk (best-effort; logged on failure).
fn save<R: tauri::Runtime>(app: &tauri::AppHandle<R>, file: &AvatarsFile) {
    config_store::save(app, AVATARS_FILE, "avatars", file);
}

/// Snapshot every custom avatar the user has set, as `id -> data-URL`.
#[tauri::command]
pub fn get_avatars(state: tauri::State<'_, AvatarsState>) -> HashMap<String, String> {
    state.0.lock().unwrap().avatars.clone()
}

/// Set or clear a custom avatar for an identity. `Some(data_url)` stores the photo;
/// `None` clears it (reverting that identity to its deterministic glyph).
#[tauri::command]
pub fn set_avatar(
    app: tauri::AppHandle,
    state: tauri::State<'_, AvatarsState>,
    id: String,
    data_url: Option<String>,
) -> Result<(), CommandError> {
    let snapshot = {
        let mut file = state.0.lock().unwrap();
        match data_url {
            Some(url) => {
                file.avatars.insert(id, url);
            }
            None => {
                file.avatars.remove(&id);
            }
        }
        file.clone()
    };
    save(&app, &snapshot);
    Ok(())
}
