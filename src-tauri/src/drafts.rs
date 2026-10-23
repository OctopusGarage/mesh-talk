//! Local text drafts: one encrypted snapshot, with the derived key reused per login.
use crate::{
    commands::CommandError,
    state::{AppState, SessionLease},
};
use mesh_talk_core::{node::MessageProtection, storage::record_log::EncryptedRecordLog};
use serde::Serialize;
use std::{collections::BTreeMap, path::Path};

type Texts = BTreeMap<String, String>;
type Protections = BTreeMap<String, MessageProtection>;
const PROTECTION_PREFIX: &str = "protection:";
const MAX_BYTES: usize = 4 * 1024 * 1024;

pub(crate) struct DraftStorage {
    lease: SessionLease,
    writer: String,
    log: EncryptedRecordLog<Texts>,
    texts: Texts,
}

#[derive(Serialize)]
pub struct DraftSnapshot {
    session: String,
    drafts: Texts,
    protections: Protections,
}

fn valid_key(key: &str) -> bool {
    (key.starts_with("account:") || key.starts_with("channel:"))
        && key.len() <= 256
        && !key.ends_with(':')
}

fn protection(value: &str) -> Result<MessageProtection, CommandError> {
    match value {
        "click" => Ok(MessageProtection::Click),
        "authenticate" => Ok(MessageProtection::Authenticate),
        _ => Err(CommandError::Validation("invalid draft protection".into())),
    }
}

// Keep MTDR01's bincode map-of-strings layout. Reserved metadata entries let legacy
// text-only snapshots decode unchanged, and text/policy share one atomic rewrite.
fn validate(texts: &Texts) -> Result<(), CommandError> {
    let bytes: usize = texts.iter().map(|(key, text)| key.len() + text.len()).sum();
    let mut conversations = std::collections::BTreeSet::new();
    if bytes > MAX_BYTES {
        return Err(CommandError::Validation(
            "draft storage limit exceeded".into(),
        ));
    }
    for (key, text) in texts {
        let conversation = if let Some(key) = key.strip_prefix(PROTECTION_PREFIX) {
            protection(text)?;
            key
        } else {
            if text.is_empty() {
                return Err(CommandError::Validation("empty draft text".into()));
            }
            key
        };
        if !valid_key(conversation) {
            return Err(CommandError::Validation(
                "invalid draft conversation".into(),
            ));
        }
        conversations.insert(conversation);
    }
    if conversations.len() > 1000 {
        return Err(CommandError::Validation(
            "draft storage limit exceeded".into(),
        ));
    }
    Ok(())
}

fn decode(texts: &Texts) -> Result<(Texts, Protections), CommandError> {
    validate(texts)?;
    let mut drafts = Texts::new();
    let mut protections = Protections::new();
    for (key, value) in texts {
        if let Some(key) = key.strip_prefix(PROTECTION_PREFIX) {
            protections.insert(key.into(), protection(value)?);
        } else {
            drafts.insert(key.clone(), value.clone());
        }
    }
    Ok((drafts, protections))
}

fn encode(mut drafts: Texts, protections: Protections) -> Result<Texts, CommandError> {
    if drafts
        .keys()
        .chain(protections.keys())
        .any(|key| !valid_key(key))
    {
        return Err(CommandError::Validation(
            "invalid draft conversation".into(),
        ));
    }
    for (key, mode) in protections {
        drafts.insert(
            format!("{PROTECTION_PREFIX}{key}"),
            match mode {
                MessageProtection::Click => "click",
                MessageProtection::Authenticate => "authenticate",
            }
            .into(),
        );
    }
    validate(&drafts)?;
    Ok(drafts)
}

fn authorize(app: &AppState, lease: &SessionLease) -> Result<(), CommandError> {
    app.session()
        .matching(lease, |_| {
            if app.app_lock().is_locked() {
                Err(CommandError::Authorization("app locked".into()))
            } else {
                Ok(())
            }
        })
        .map_err(CommandError::Authorization)?
}

fn load(app: &AppState, lease: &SessionLease, root: &Path) -> Result<DraftSnapshot, CommandError> {
    authorize(app, lease)?;
    let mut cache = app
        .drafts
        .lock()
        .map_err(|_| CommandError::Internal("draft lock unavailable".into()))?;
    if cache.as_ref().is_none_or(|stored| stored.lease != *lease) {
        let path = crate::config_store::account_path(root, lease.owner(), "drafts.enc")
            .ok_or_else(|| CommandError::Validation("invalid draft owner".into()))?;
        if path
            .metadata()
            .is_ok_and(|meta| meta.len() > (MAX_BYTES + 65536) as u64)
        {
            return Err(CommandError::Validation("draft file exceeds limit".into()));
        }
        let password = app
            .session()
            .matching(lease, |info| info.password.clone())
            .map_err(CommandError::Authorization)?;
        // Password derivation and IO run on a blocking worker, outside the session lock.
        let (log, records) = EncryptedRecordLog::<Texts>::open(&path, &password, b"MTDR01")
            .map_err(|error| CommandError::Io(error.to_string()))?;
        let texts = records.into_iter().last().unwrap_or_default();
        validate(&texts)?;
        authorize(app, lease)?;
        *cache = Some(DraftStorage {
            lease: lease.clone(),
            writer: rand::random::<u128>().to_string(),
            log,
            texts,
        });
    }
    app.session()
        .matching(lease, |_| {
            if app.app_lock().is_locked() {
                return Err(CommandError::Authorization("app locked".into()));
            }
            let storage = cache.as_ref().unwrap();
            let (drafts, protections) = decode(&storage.texts)?;
            Ok(DraftSnapshot {
                session: storage.writer.clone(),
                drafts,
                protections,
            })
        })
        .map_err(CommandError::Authorization)?
}

fn save(
    app: &AppState,
    lease: &SessionLease,
    writer: &str,
    texts: Texts,
) -> Result<(), CommandError> {
    validate(&texts)?;
    let mut cache = app
        .drafts
        .lock()
        .map_err(|_| CommandError::Internal("draft lock unavailable".into()))?;
    app.session()
        .matching(lease, |_| {
            if app.app_lock().is_locked() {
                return Err(CommandError::Authorization("app locked".into()));
            }
            let storage = cache
                .as_mut()
                .filter(|stored| stored.lease == *lease && stored.writer == writer)
                .ok_or_else(|| CommandError::Authorization("draft session not loaded".into()))?;
            if storage.texts != texts {
                storage
                    .log
                    .rewrite(std::slice::from_ref(&texts))
                    .map_err(|error| CommandError::Io(error.to_string()))?;
                storage.texts = texts;
            }
            Ok(())
        })
        .map_err(CommandError::Authorization)?
}

fn capture(app: &AppState, owner: &str) -> Result<SessionLease, CommandError> {
    let lease = app
        .session()
        .capture()
        .map_err(CommandError::Authorization)?;
    if lease.owner() != owner {
        return Err(CommandError::Authorization("draft owner mismatch".into()));
    }
    authorize(app, &lease)?;
    Ok(lease)
}

#[tauri::command]
pub async fn load_conversation_drafts(
    app: tauri::State<'_, AppState>,
    owner: String,
) -> Result<DraftSnapshot, CommandError> {
    let lease = capture(&app, &owner)?;
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || load(&app, &lease, &crate::data_dir()))
        .await
        .map_err(|error| CommandError::Internal(error.to_string()))?
}

#[tauri::command]
pub async fn save_conversation_drafts(
    app: tauri::State<'_, AppState>,
    owner: String,
    session: String,
    drafts: Texts,
    protections: Option<Protections>,
) -> Result<(), CommandError> {
    let drafts = encode(drafts, protections.unwrap_or_default())?;
    let lease = capture(&app, &owner)?;
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || save(&app, &lease, &session, drafts))
        .await
        .map_err(|error| CommandError::Internal(error.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{auth_service::AuthService, user::User};
    use mesh_talk_core::{identity::manager::IdentityManager, storage::file_manager::FileManager};
    use std::sync::Arc;

    fn fixture(root: &Path) -> AppState {
        let app = AppState::new(AuthService::new(Arc::new(IdentityManager::new(
            FileManager::new(root.to_owned()),
        ))));
        login(&app);
        app
    }
    fn login(app: &AppState) {
        app.session().set(
            "token".into(),
            User {
                user_id: "alice".into(),
                name: "alice".into(),
                display_name: "Alice".into(),
                address: "fixture".into(),
                created_at: 0,
                last_seen: 0,
                is_online: true,
            },
            "password".into(),
        );
    }
    #[test]
    fn encrypted_restart_restores_text_and_clearing_replaces_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let app = fixture(root.path());
        let lease = capture(&app, "alice").unwrap();
        let snapshot = load(&app, &lease, root.path()).unwrap();
        assert!(snapshot.drafts.is_empty());
        let texts = Texts::from([("account:bob".into(), "秘密\nkeep this draft".into())]);
        save(&app, &lease, &snapshot.session, texts.clone()).unwrap();
        let path = crate::config_store::account_path(root.path(), "alice", "drafts.enc").unwrap();
        let raw = std::fs::read(path).unwrap();
        assert!(
            !raw.windows(b"keep this draft".len())
                .any(|bytes| bytes == b"keep this draft")
        );
        drop(app);
        let restarted = fixture(root.path());
        let lease = capture(&restarted, "alice").unwrap();
        let snapshot = load(&restarted, &lease, root.path()).unwrap();
        assert_eq!(snapshot.drafts, texts);
        assert!(snapshot.protections.is_empty());
        save(&restarted, &lease, &snapshot.session, Texts::new()).unwrap();
        drop(restarted);
        let restarted = fixture(root.path());
        assert!(
            load(
                &restarted,
                &capture(&restarted, "alice").unwrap(),
                root.path()
            )
            .unwrap()
            .drafts
            .is_empty()
        );
    }
    #[test]
    fn replaced_sessions_and_wrong_owners_cannot_read_or_write() {
        let root = tempfile::tempdir().unwrap();
        let app = fixture(root.path());
        let lease = capture(&app, "alice").unwrap();
        load(&app, &lease, root.path()).unwrap();
        assert!(capture(&app, "bob").is_err());
        login(&app);
        assert!(load(&app, &lease, root.path()).is_err());
        assert!(save(&app, &lease, "old", Texts::new()).is_err());
        let fresh = capture(&app, "alice").unwrap();
        assert!(save(&app, &fresh, "old", Texts::new()).is_err());
    }
    #[test]
    fn registered_commands_validate_session_and_lock_before_plaintext_release() {
        let root = tempfile::tempdir().unwrap();
        let state = fixture(root.path());
        let lease = capture(&state, "alice").unwrap();
        load(&state, &lease, root.path()).unwrap();
        let app = tauri::test::mock_builder()
            .manage(state.clone())
            .manage(state.app_lock().clone())
            .invoke_handler(tauri::generate_handler![
                load_conversation_drafts,
                save_conversation_drafts,
                crate::app_lock::lock_app
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let invoke = |cmd: &str, body: serde_json::Value| {
            tauri::test::get_ipc_response(
                &webview,
                tauri::webview::InvokeRequest {
                    cmd: cmd.into(),
                    callback: tauri::ipc::CallbackFn(0),
                    error: tauri::ipc::CallbackFn(1),
                    url: if cfg!(windows) {
                        "http://tauri.localhost"
                    } else {
                        "tauri://localhost"
                    }
                    .parse()
                    .unwrap(),
                    body: tauri::ipc::InvokeBody::Json(body),
                    headers: Default::default(),
                    invoke_key: tauri::test::INVOKE_KEY.into(),
                },
            )
            .map(|body| body.deserialize::<serde_json::Value>().unwrap())
        };
        let snapshot = invoke(
            "load_conversation_drafts",
            serde_json::json!({"owner":"alice"}),
        )
        .unwrap();
        assert!(!snapshot["session"].as_str().unwrap().is_empty());
        assert!(
            invoke(
                "save_conversation_drafts",
                serde_json::json!({"owner":"alice", "session":"stale", "drafts":{}})
            )
            .is_err()
        );
        assert!(
            invoke(
                "load_conversation_drafts",
                serde_json::json!({"owner":"bob"})
            )
            .is_err()
        );
        invoke("save_conversation_drafts", serde_json::json!({"owner":"alice", "session": snapshot["session"], "drafts":{"account:bob":"keep"}})).unwrap();
        assert_eq!(
            invoke(
                "load_conversation_drafts",
                serde_json::json!({"owner":"alice"})
            )
            .unwrap()["drafts"]["account:bob"],
            "keep"
        );
        invoke("save_conversation_drafts", serde_json::json!({"owner":"alice", "session": snapshot["session"], "drafts":{"account:bob":"private"}, "protections":{"account:bob":"authenticate"}})).unwrap();
        let protected = invoke(
            "load_conversation_drafts",
            serde_json::json!({"owner":"alice"}),
        )
        .unwrap();
        assert_eq!(protected["drafts"]["account:bob"], "private");
        assert_eq!(protected["protections"]["account:bob"], "authenticate");
        invoke("lock_app", serde_json::json!({})).unwrap();
        assert!(state.drafts.lock().unwrap().is_none());
        assert!(
            invoke(
                "load_conversation_drafts",
                serde_json::json!({"owner":"alice"})
            )
            .is_err()
        );
        assert!(
            invoke(
                "save_conversation_drafts",
                serde_json::json!({"owner":"alice", "session": snapshot["session"], "drafts":{}})
            )
            .is_err()
        );
        // Unlock keeps the login lease, but must retire the old draft writer.
        state.app_lock().clear();
        let restored = load(&state, &lease, root.path()).unwrap();
        assert_ne!(restored.session, snapshot["session"].as_str().unwrap());
        assert!(
            invoke(
                "save_conversation_drafts",
                serde_json::json!({"owner":"alice", "session": snapshot["session"], "drafts":{}})
            )
            .is_err()
        );
    }
    #[test]
    fn protected_policy_uses_the_existing_encrypted_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let app = fixture(root.path());
        let lease = capture(&app, "alice").unwrap();
        let snapshot = load(&app, &lease, root.path()).unwrap();
        let stored = Texts::from([
            ("account:bob".into(), "secret".into()),
            ("protection:account:bob".into(), "authenticate".into()),
            ("protection:channel:team".into(), "click".into()),
        ]);
        save(&app, &lease, &snapshot.session, stored).unwrap();
        drop(app);
        let restarted = fixture(root.path());
        let snapshot = load(
            &restarted,
            &capture(&restarted, "alice").unwrap(),
            root.path(),
        )
        .unwrap();
        assert_eq!(
            snapshot.drafts,
            Texts::from([("account:bob".into(), "secret".into())])
        );
        assert_eq!(
            snapshot.protections.get("account:bob"),
            Some(&MessageProtection::Authenticate)
        );
        assert_eq!(
            snapshot.protections.get("channel:team"),
            Some(&MessageProtection::Click)
        );
        assert!(
            validate(&Texts::from([(
                "protection:account:bob".into(),
                "unknown".into()
            )]))
            .is_err()
        );
        assert!(validate(&Texts::from([("protection:wrong".into(), "click".into())])).is_err());
    }

    #[test]
    fn invalid_and_oversized_snapshots_are_rejected() {
        assert!(validate(&Texts::from([("wrong".into(), "text".into())])).is_err());
        assert!(validate(&Texts::from([("account:a".into(), "x".repeat(MAX_BYTES))])).is_err());
        assert!(validate(&Texts::from([("account:a".into(), "".into())])).is_err());
    }
}
