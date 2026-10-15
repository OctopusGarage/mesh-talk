//! Desktop IPC commands for linked-device synchronization and account preferences.

use super::NodeState;
use crate::commands::CommandError;
use mesh_talk_core::node::{AccountPreference, DeviceSyncPolicy, DeviceSyncStatus, SyncCategory};

/// Return the local account's synchronization category policy.
#[tauri::command]
pub async fn get_device_sync_policy(
    state: tauri::State<'_, NodeState>,
) -> Result<DeviceSyncPolicy, CommandError> {
    Ok(state.node_handle().await?.device_sync_policy())
}

/// Return per-device synchronization status for the local account.
#[tauri::command]
pub async fn get_device_sync_status(
    state: tauri::State<'_, NodeState>,
) -> Result<Vec<DeviceSyncStatus>, CommandError> {
    Ok(state.node_handle().await?.device_sync_status())
}

/// Enable or disable one category of linked-device synchronization.
#[tauri::command]
pub async fn set_device_sync_category(
    state: tauri::State<'_, NodeState>,
    category: SyncCategory,
    enabled: bool,
) -> Result<DeviceSyncPolicy, CommandError> {
    let node = state.node_handle().await?;
    node.set_device_sync_category(category, enabled)
        .map_err(CommandError::from)?;
    Ok(node.device_sync_policy())
}

/// List synchronized preferences in the requested category.
#[tauri::command]
pub async fn get_account_preferences(
    state: tauri::State<'_, NodeState>,
    category: SyncCategory,
) -> Result<Vec<AccountPreference>, CommandError> {
    Ok(state.node_handle().await?.account_preferences(category))
}

/// Set or delete one synchronized account preference.
#[tauri::command]
pub async fn set_account_preference(
    state: tauri::State<'_, NodeState>,
    category: SyncCategory,
    key: String,
    value: Option<String>,
) -> Result<AccountPreference, CommandError> {
    state
        .node_handle()
        .await?
        .set_account_preference(category, key, value)
        .map_err(CommandError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::Manager;

    #[tokio::test]
    async fn sync_commands_read_and_update_the_managed_node() {
        let root = tempfile::tempdir().unwrap();
        let managed = NodeState::empty();
        let app = tauri::test::mock_builder()
            .manage(managed.clone())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let state = app.state::<NodeState>();

        assert!(get_device_sync_policy(state.clone()).await.is_err());
        assert!(get_device_sync_status(state.clone()).await.is_err());

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
        *managed.0.lock().await = Some(runtime);

        let original = get_device_sync_policy(state.clone()).await.unwrap();
        assert!(original.history);
        assert!(
            get_device_sync_status(state.clone())
                .await
                .unwrap()
                .is_empty()
        );
        let changed = set_device_sync_category(state.clone(), SyncCategory::History, false)
            .await
            .unwrap();
        assert!(!changed.history);
        assert_eq!(
            get_device_sync_policy(state.clone()).await.unwrap(),
            changed
        );

        assert!(
            get_account_preferences(state.clone(), SyncCategory::Appearance)
                .await
                .unwrap()
                .is_empty()
        );
        let preference = set_account_preference(
            state.clone(),
            SyncCategory::Appearance,
            "theme".into(),
            Some("dark".into()),
        )
        .await
        .unwrap();
        assert_eq!(preference.value.as_deref(), Some("dark"));
        assert_eq!(
            get_account_preferences(state.clone(), SyncCategory::Appearance)
                .await
                .unwrap(),
            vec![preference]
        );
        assert!(
            set_account_preference(
                state,
                SyncCategory::Appearance,
                "invalid key".into(),
                Some("dark".into()),
            )
            .await
            .is_err()
        );
        managed.0.lock().await.take().unwrap().stop().await;
    }
}
