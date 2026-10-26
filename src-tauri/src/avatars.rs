//! Private avatar overrides share the account-document owner. Public own avatars
//! retain the signed profile owner; legacy JSON is only an explicit source import.
use crate::{
    chat_commands::NodeState, commands::CommandError, config_store,
    runtime_admission::RuntimeAdmission, state::AppState,
};
use base64::Engine;
use mesh_talk_core::node::{
    AccountDocumentDomain, AccountDocumentKey, DocumentConversation, Node, NodeRuntime,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct AvatarsFile {
    #[serde(default)]
    avatars: HashMap<String, String>,
}
#[derive(Clone, Default)]
pub struct AvatarsState {
    root: Option<PathBuf>,
}
impl AvatarsState {
    fn path(&self, owner: &str) -> Result<PathBuf, CommandError> {
        config_store::account_path(
            &self.root.clone().unwrap_or_else(crate::data_dir),
            owner,
            AVATARS_FILE,
        )
        .ok_or_else(|| CommandError::Authorization("invalid owner".into()))
    }
    #[cfg(test)]
    pub(crate) fn isolated(root: PathBuf) -> Self {
        Self { root: Some(root) }
    }
}
const AVATARS_FILE: &str = "avatars.json";
fn target(id: &str) -> Result<DocumentConversation, CommandError> {
    if ![32, 64].contains(&id.len())
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(CommandError::Validation("invalid avatar identity".into()));
    }
    Ok(if id.len() == 32 {
        DocumentConversation::Account(id.into())
    } else {
        DocumentConversation::Channel(id.into())
    })
}
fn id(target: &DocumentConversation) -> &str {
    match target {
        DocumentConversation::Account(id) | DocumentConversation::Channel(id) => id,
    }
}
fn valid_url(url: &str) -> bool {
    if url.len() > 1024 * 1024 {
        return false;
    }
    let Some((mime, encoded)) = url.split_once(";base64,") else {
        return false;
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return false;
    };
    match mime {
        "data:image/png" => bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]),
        "data:image/jpeg" => bytes.starts_with(&[255, 216, 255]),
        "data:image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "data:image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        _ => false,
    }
}
fn check(runtime: &NodeRuntime, expected: &str) -> Result<(), CommandError> {
    if runtime.account_id() != expected {
        return Err(CommandError::Authorization(
            "avatar belongs to another account".into(),
        ));
    }
    Ok(())
}
fn legacy(path: &Path) -> HashMap<String, String> {
    if std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= 128 * 1024 * 1024) {
        config_store::read_account::<AvatarsFile>(path).avatars
    } else {
        HashMap::new()
    }
}
fn project(
    node: &Node,
    mut values: HashMap<String, String>,
) -> Result<HashMap<String, String>, CommandError> {
    let mut after = None;
    loop {
        let page =
            node.account_documents(AccountDocumentDomain::AvatarOverride, after.as_ref(), 64)?;
        for meta in page.items {
            let AccountDocumentKey::AvatarOverride { target } = &meta.key else {
                continue;
            };
            values.remove(id(target));
            if !meta.tombstone
                && let Some(document) = node.account_document(&meta.key)?
                && let Ok(url) = std::str::from_utf8(document.payload())
                && valid_url(url)
            {
                values.insert(id(target).to_owned(), url.to_owned());
            }
        }
        after = page.next;
        if after.is_none() {
            break;
        }
    }
    Ok(values)
}

#[tauri::command]
pub async fn get_avatars(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, AvatarsState>,
    node: tauri::State<'_, NodeState>,
    owner: String,
    expected_account: Option<String>,
) -> Result<HashMap<String, String>, CommandError> {
    let Some(expected) = expected_account else {
        // Unbound legacy files have no cryptographic-account provenance. Bootstrap
        // cannot attribute them after adoption; only explicit kept-source import can.
        return config_store::with_account(&app, &owner, AVATARS_FILE, |_| Ok(HashMap::new()));
    };
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let _ = state;
    admission
        .blocking(move |runtime, _| {
            check(runtime, &expected)?;
            project(&runtime.handle(), HashMap::new())
        })
        .await
}

#[tauri::command]
pub async fn set_avatar(
    app: tauri::State<'_, AppState>,
    _state: tauri::State<'_, AvatarsState>,
    node: tauri::State<'_, NodeState>,
    owner: String,
    expected_account: String,
    id: String,
    data_url: Option<String>,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let key = AccountDocumentKey::AvatarOverride {
        target: target(&id)?,
    };
    if data_url.as_ref().is_some_and(|url| !valid_url(url)) {
        return Err(CommandError::Validation("invalid avatar image".into()));
    }
    admission
        .blocking(move |runtime, _| {
            check(runtime, &expected_account)?;
            let node = runtime.handle();
            let current = node.account_document_meta(&key);
            match data_url.as_ref() {
                Some(url) => {
                    node.put_account_document_if(
                        key,
                        url.as_bytes().to_vec(),
                        current.map(|meta| meta.version()),
                        |commit| commit(),
                    )?;
                }
                None => {
                    if current.as_ref().is_none_or(|meta| !meta.tombstone) {
                        node.remove_account_document_if(
                            key,
                            current.map(|meta| meta.version()),
                            |commit| commit(),
                        )?;
                    }
                }
            }
            Ok(())
        })
        .await
}

#[tauri::command]
pub async fn publish_legacy_avatar_overrides(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, AvatarsState>,
    node: tauri::State<'_, NodeState>,
    owner: String,
    expected_account: String,
) -> Result<usize, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let path = state.path(&owner)?;
    admission
        .blocking(move |runtime, _| {
            check(runtime, &expected_account)?;
            let node = runtime.handle();
            let mut imported = 0;
            for (identity, url) in legacy(&path) {
                // Our public photo is already owned and synchronized by the signed profile.
                if identity == expected_account || !valid_url(&url) {
                    continue;
                }
                let Ok(target) = target(&identity) else {
                    continue;
                };
                let key = AccountDocumentKey::AvatarOverride { target };
                if node.account_document_meta(&key).is_some() {
                    continue;
                }
                match node
                    .put_account_document_if(key.clone(), url.into_bytes(), None, |commit| commit())
                {
                    Ok(_) => imported += 1,
                    Err(_) if node.account_document_meta(&key).is_some() => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(imported)
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overrides_accept_only_bounded_raster_data_and_typed_targets() {
        assert!(valid_url("data:image/png;base64,iVBORw0KGgo="));
        for url in [
            "https://example.com/photo.png",
            "data:image/svg+xml;base64,PHN2Zy8+",
            "data:image/png;base64,bm90LXBvcnRhYmxl",
        ] {
            assert!(!valid_url(url));
        }
        assert!(!valid_url(&format!(
            "data:image/png;base64,{}",
            "A".repeat(1024 * 1024)
        )));
        assert!(target(&"ab".repeat(16)).is_ok());
        assert!(target(&"ab".repeat(32)).is_ok());
        assert!(target("../../account").is_err());
    }
}
