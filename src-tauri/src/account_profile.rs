//! Session-bound public profile commands. Identity metadata is never caller supplied.

use crate::{
    chat_commands::NodeState, commands::CommandError, runtime_admission::RuntimeAdmission,
    state::AppState,
};
use serde::Serialize;

#[derive(Serialize)]
pub struct AccountProfileView {
    account_id: String,
    display_name: String,
    bio: String,
    created_at_ms: Option<u64>,
}

#[tauri::command]
pub async fn get_account_profile(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    owner: String,
    account_id: String,
) -> Result<Option<AccountProfileView>, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &state, &owner)?;
    if account_id.len() != 32 || hex::decode(&account_id).is_err() {
        return Err(CommandError::Validation("invalid mesh account ID".into()));
    }
    admission
        .blocking(move |runtime, _| {
            let node = runtime.handle();
            if let Some(profile) = node.account_profile(&account_id) {
                return Ok(Some(AccountProfileView {
                    account_id: profile.account_id(),
                    display_name: profile.display_name,
                    bio: profile.bio,
                    created_at_ms: profile.created_at_ms,
                }));
            }
            // Legacy statements remain readable; their hand-entered dates are not account metadata.
            Ok(node.trust_card(&account_id).map(legacy_profile_view))
        })
        .await
}

#[tauri::command]
pub async fn save_account_bio(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    owner: String,
    bio: String,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture(&app, &state, &owner)?;
    let (completed, response) = tokio::sync::oneshot::channel();
    let completed = std::sync::Arc::new(std::sync::Mutex::new(Some(completed)));
    let fallback = completed.clone();
    tauri::async_runtime::spawn(async move {
        let result = admission
            .detached(move |runtime, admission| {
                Box::pin(async move {
                    runtime
                        .handle()
                        .save_account_bio_if(bio, |commit| {
                            admission.authorize_accept(runtime, commit)?;
                            // Reply after durable local acceptance; peer fanout retains admission.
                            if let Some(completed) =
                                completed.lock().expect("profile reply lock").take()
                            {
                                let _ = completed.send(Ok(()));
                            }
                            Ok(())
                        })
                        .await
                        .map_err(CommandError::from)
                })
            })
            .await;
        if let Some(completed) = fallback.lock().expect("profile reply lock").take() {
            let _ = completed.send(result);
        }
    });
    response
        .await
        .map_err(|_| CommandError::Internal("profile save terminated".into()))?
}

fn legacy_profile_view(card: mesh_talk_core::node::TrustCard) -> AccountProfileView {
    AccountProfileView {
        account_id: card.account_id(),
        display_name: card.display_name,
        bio: card.bio,
        created_at_ms: None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_profile_preserves_about_without_promoting_a_claimed_creation_date() {
        let account = mesh_talk_core::identity::account::Account::generate();
        let card = mesh_talk_core::node::TrustCard::new(
            &account,
            1,
            "Alice".into(),
            "Existing bio".into(),
            Some(42),
            None,
        );
        let view = legacy_profile_view(card);
        assert_eq!(view.bio, "Existing bio");
        assert_eq!(view.display_name, "Alice");
        assert_eq!(view.account_id, account.account_id());
        assert_eq!(view.created_at_ms, None);
    }
}
