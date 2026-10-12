//! Custom avatars: per-identity profile photos the user sets locally. A user can attach
//! a custom image for THEMSELVES or for any CONTACT, replacing the deterministic
//! `IdentityGlyph` wherever that identity renders. This is purely local UI state (no
//! crypto, no protocol), persisted as JSON in the signed-in host account's profile.
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
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The on-disk shape: `id` (account_id / channel_id) -> data-URL string.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct AvatarsFile {
    #[serde(default)]
    avatars: HashMap<String, String>,
}

/// Serializes avatar table read/modify/write operations within the process.
#[derive(Clone, Default)]
pub struct AvatarsState(Arc<Mutex<()>>);

/// The persistence file inside the host account's profile.
const AVATARS_FILE: &str = "avatars.json";

/// Snapshot every custom avatar the user has set, as `id -> data-URL`.
#[tauri::command]
pub fn get_avatars(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, AvatarsState>,
    owner: String,
) -> Result<HashMap<String, String>, CommandError> {
    config_store::with_account(&app, &owner, AVATARS_FILE, |path| {
        let _guard = state.0.lock().unwrap();
        let file: AvatarsFile = config_store::read_account(path);
        Ok(file.avatars)
    })
}

/// Set or clear a custom avatar for an identity. `Some(data_url)` stores the photo;
/// `None` clears it (reverting that identity to its deterministic glyph).
#[tauri::command]
pub fn set_avatar(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, AvatarsState>,
    owner: String,
    id: String,
    data_url: Option<String>,
) -> Result<(), CommandError> {
    config_store::with_account(&app, &owner, AVATARS_FILE, |path| {
        let _guard = state.0.lock().unwrap();
        let mut file: AvatarsFile = config_store::read_account(path);
        match data_url {
            Some(url) => {
                file.avatars.insert(id, url);
            }
            None => {
                file.avatars.remove(&id);
            }
        }
        config_store::write_account(path, &file)
    })
}
