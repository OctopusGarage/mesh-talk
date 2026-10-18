//! Owner-scoped access to the core's authenticated incoming-DM language preference.
use crate::{
    chat_commands::NodeState, commands::CommandError, runtime_admission::RuntimeAdmission,
    state::AppState,
};
use mesh_talk_core::node::{LanguageVerdict, MIN_TARGET_PERCENT, SUPPORTED_LANGUAGES};
use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct LanguagePreference {
    language: Option<String>,
    min_target_percent: u8,
    supported_languages: &'static [&'static str],
}

fn valid_account(account: &str) -> Result<(), CommandError> {
    if account.len() != 32 || !account.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CommandError::InvalidInput("invalid account id".into()));
    }
    Ok(())
}

fn preference(node: &mesh_talk_core::node::Node, account: &str) -> LanguagePreference {
    LanguagePreference {
        language: node.dm_language_policy(account),
        min_target_percent: MIN_TARGET_PERCENT,
        supported_languages: SUPPORTED_LANGUAGES,
    }
}

#[tauri::command]
pub(crate) async fn dm_language_policy(
    owner: String,
    account: Option<String>,
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
) -> Result<LanguagePreference, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &state, &owner)?;
    if let Some(account) = &account {
        valid_account(account)?;
    }
    admission
        .with_runtime(|runtime| {
            let node = runtime.handle();
            Ok(preference(
                &node,
                account.as_deref().unwrap_or(&node.account_id()),
            ))
        })
        .await
}

#[tauri::command]
pub(crate) async fn set_dm_language_policy(
    owner: String,
    language: Option<String>,
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
) -> Result<LanguagePreference, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &state, &owner)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let node = runtime.handle();
                node.set_dm_language_policy_if(language, |accept| {
                    admission.authorize_accept(runtime, accept)
                })
                .await
                .map_err(CommandError::from)?;
                admission.checked(runtime, || Ok(preference(&node, &node.account_id())))
            })
        })
        .await
}

#[tauri::command]
pub(crate) async fn validate_dm_language(
    owner: String,
    account: String,
    text: String,
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
) -> Result<LanguageVerdict, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &state, &owner)?;
    valid_account(&account)?;
    // Classification is CPU work; don't hold the async state mutex on the executor.
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let node = runtime.handle();
                let verdict = tokio::task::spawn_blocking(move || {
                    node.validate_dm_language(&account, text.as_bytes())
                })
                .await
                .map_err(|error| CommandError::Internal(error.to_string()))?
                .map_err(CommandError::from)?;
                admission.checked(runtime, || Ok(verdict))
            })
        })
        .await
}
