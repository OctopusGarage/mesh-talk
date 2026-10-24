//! Desktop IPC commands for linked-device synchronization and account preferences.

use super::NodeState;
use crate::commands::CommandError;
use crate::{runtime_admission::RuntimeAdmission, state::AppState};
use mesh_talk_core::node::{AccountPreference, DeviceSyncPolicy, DeviceSyncStatus, SyncCategory};

/// Credential installation is independent of optional history synchronization.
#[tauri::command]
pub async fn get_account_login_sync_status(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    owner: String,
) -> Result<crate::shared_account_login::AccountLoginSyncStatus, CommandError> {
    let root = crate::recovery::data_root(&state);
    RuntimeAdmission::capture(&app, &state, &owner)?
        .with_runtime(|runtime| crate::shared_account_login::sync_status(&root, &owner, runtime))
        .await
}

/// Return the local account's synchronization category policy.
#[tauri::command]
pub async fn get_device_sync_policy(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    owner: String,
) -> Result<DeviceSyncPolicy, CommandError> {
    RuntimeAdmission::capture(&app, &state, &owner)?
        .with_runtime(|runtime| Ok(runtime.handle().device_sync_policy()))
        .await
}

/// Return per-device synchronization status for the local account.
#[tauri::command]
pub async fn get_device_sync_status(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    owner: String,
) -> Result<Vec<DeviceSyncStatus>, CommandError> {
    RuntimeAdmission::capture(&app, &state, &owner)?
        .with_runtime(|runtime| Ok(runtime.handle().device_sync_status()))
        .await
}

/// Enable or disable one category of linked-device synchronization.
#[tauri::command]
pub async fn set_device_sync_category(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    category: SyncCategory,
    enabled: bool,
    owner: String,
) -> Result<DeviceSyncPolicy, CommandError> {
    RuntimeAdmission::capture(&app, &state, &owner)?
        .with_runtime(|runtime| {
            let node = runtime.handle();
            node.set_device_sync_category(category, enabled)
                .map_err(CommandError::from)?;
            Ok(node.device_sync_policy())
        })
        .await
}

/// List synchronized preferences in the requested category.
#[tauri::command]
pub async fn get_account_preferences(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    category: SyncCategory,
    owner: String,
) -> Result<Vec<AccountPreference>, CommandError> {
    RuntimeAdmission::capture(&app, &state, &owner)?
        .with_runtime(|runtime| Ok(runtime.handle().account_preferences(category)))
        .await
}

/// Set or delete one synchronized account preference.
#[tauri::command]
pub async fn set_account_preference(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    category: SyncCategory,
    key: String,
    value: Option<String>,
    owner: String,
    expected_account: String,
) -> Result<AccountPreference, CommandError> {
    RuntimeAdmission::capture(&app, &state, &owner)?
        .with_runtime(|runtime| {
            if runtime.account_id() != expected_account {
                return Err(CommandError::Authorization(
                    "Preference belongs to another account".into(),
                ));
            }
            runtime
                .handle()
                .set_account_preference(category, key, value)
                .map_err(CommandError::from)
        })
        .await
}

/// Seed an absent account preference atomically; existing values and clears win.
#[tauri::command]
pub async fn seed_account_preference(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    category: SyncCategory,
    key: String,
    value: Option<String>,
    owner: String,
    expected_account: String,
) -> Result<AccountPreference, CommandError> {
    RuntimeAdmission::capture(&app, &state, &owner)?
        .with_runtime(|runtime| {
            if runtime.account_id() != expected_account {
                return Err(CommandError::Authorization(
                    "Preference belongs to another account".into(),
                ));
            }
            runtime
                .handle()
                .set_account_preference_if_absent(category, key, value)
                .map_err(CommandError::from)
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::Manager;

    #[tokio::test]
    async fn sync_commands_read_and_update_the_managed_node() {
        let root = tempfile::tempdir().unwrap();
        let managed = NodeState::empty();
        let app_state = AppState::new(crate::services::auth_service::AuthService::new(
            std::sync::Arc::new(mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
            )),
        ));
        app_state.session().set(
            "test".into(),
            crate::services::user::User {
                user_id: "sync-test".into(),
                account_login_name: None,
                name: "sync-test".into(),
                display_name: "Sync Test".into(),
                address: "fixture".into(),
                created_at: 0,
                last_seen: 0,
                is_online: true,
            },
            "test-password".into(),
        );
        let app = tauri::test::mock_builder()
            .manage(managed.clone())
            .manage(app_state)
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let state = app.state::<NodeState>();

        assert!(
            get_device_sync_policy(app.state::<AppState>(), state.clone(), "sync-test".into())
                .await
                .is_err()
        );
        assert!(
            get_device_sync_status(app.state::<AppState>(), state.clone(), "sync-test".into())
                .await
                .is_err()
        );
        assert!(
            set_device_sync_category(
                app.state::<AppState>(),
                state.clone(),
                SyncCategory::History,
                false,
                "sync-test".into()
            )
            .await
            .is_err()
        );
        assert!(
            get_account_preferences(
                app.state::<AppState>(),
                state.clone(),
                SyncCategory::Contacts,
                "sync-test".into()
            )
            .await
            .is_err()
        );
        assert!(
            set_account_preference(
                app.state::<AppState>(),
                state.clone(),
                SyncCategory::Contacts,
                "alias".into(),
                Some("Alice".into()),
                "sync-test".into(),
                "pending-account".into()
            )
            .await
            .is_err()
        );

        let runtime = mesh_talk_core::node::NodeRuntime::start(
            root.path(),
            "sync-test",
            "Sync Test",
            "test-password",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        )
        .await
        .unwrap();
        *managed.0.write().await = Some(runtime);

        // A webview command queued by a previous login must not borrow the
        // replacement account's runtime, even when it starts after the switch.
        assert!(matches!(
            get_device_sync_policy(
                app.state::<AppState>(),
                state.clone(),
                "previous-owner".into()
            )
            .await,
            Err(CommandError::Authorization(_))
        ));
        assert!(matches!(
            set_account_preference(
                app.state::<AppState>(),
                state.clone(),
                SyncCategory::Appearance,
                "theme".into(),
                Some("previous-owner-value".into()),
                "previous-owner".into(),
                "previous-crypto-account".into()
            )
            .await,
            Err(CommandError::Authorization(_))
        ));

        let expected_account = state
            .0
            .read()
            .await
            .as_ref()
            .unwrap()
            .account_id()
            .to_owned();
        assert!(
            set_account_preference(
                app.state::<AppState>(),
                state.clone(),
                SyncCategory::Contacts,
                "favorite:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                Some("old account".into()),
                "sync-test".into(),
                "previous-crypto-account".into(),
            )
            .await
            .is_err()
        );

        let original =
            get_device_sync_policy(app.state::<AppState>(), state.clone(), "sync-test".into())
                .await
                .unwrap();
        assert!(original.history);
        assert!(
            get_device_sync_status(app.state::<AppState>(), state.clone(), "sync-test".into())
                .await
                .unwrap()
                .is_empty()
        );
        let changed = set_device_sync_category(
            app.state::<AppState>(),
            state.clone(),
            SyncCategory::History,
            false,
            "sync-test".into(),
        )
        .await
        .unwrap();
        assert!(!changed.history);
        assert_eq!(
            get_device_sync_policy(app.state::<AppState>(), state.clone(), "sync-test".into())
                .await
                .unwrap(),
            changed
        );

        assert!(
            get_account_preferences(
                app.state::<AppState>(),
                state.clone(),
                SyncCategory::Appearance,
                "sync-test".into()
            )
            .await
            .unwrap()
            .is_empty()
        );
        let preference = set_account_preference(
            app.state::<AppState>(),
            state.clone(),
            SyncCategory::Appearance,
            "theme".into(),
            Some("dark".into()),
            "sync-test".into(),
            expected_account.clone(),
        )
        .await
        .unwrap();
        assert_eq!(preference.value.as_deref(), Some("dark"));
        assert_eq!(
            get_account_preferences(
                app.state::<AppState>(),
                state.clone(),
                SyncCategory::Appearance,
                "sync-test".into()
            )
            .await
            .unwrap(),
            vec![preference]
        );
        assert!(
            set_account_preference(
                app.state::<AppState>(),
                state,
                SyncCategory::Appearance,
                "invalid key".into(),
                Some("dark".into()),
                "sync-test".into(),
                expected_account.clone()
            )
            .await
            .is_err()
        );
        assert!(
            set_account_preference(
                app.state::<AppState>(),
                app.state::<NodeState>(),
                SyncCategory::History,
                "history".into(),
                Some("enabled".into()),
                "sync-test".into(),
                expected_account.clone()
            )
            .await
            .is_err()
        );
        managed.0.write().await.take().unwrap().stop().await;
    }
}
