use super::NodeState;
use crate::commands::CommandError;
use mesh_talk_core::node::{AccountPreference, DeviceSyncPolicy, DeviceSyncStatus, SyncCategory};

#[tauri::command]
pub async fn get_device_sync_policy(
    state: tauri::State<'_, NodeState>,
) -> Result<DeviceSyncPolicy, CommandError> {
    Ok(state.node_handle().await?.device_sync_policy())
}

#[tauri::command]
pub async fn get_device_sync_status(
    state: tauri::State<'_, NodeState>,
) -> Result<Vec<DeviceSyncStatus>, CommandError> {
    Ok(state.node_handle().await?.device_sync_status())
}

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

#[tauri::command]
pub async fn get_account_preferences(
    state: tauri::State<'_, NodeState>,
    category: SyncCategory,
) -> Result<Vec<AccountPreference>, CommandError> {
    Ok(state.node_handle().await?.account_preferences(category))
}

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
