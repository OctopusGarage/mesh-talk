use super::NodeState;
use crate::commands::CommandError;
use mesh_talk_core::node::{DeviceSyncPolicy, SyncCategory};

#[tauri::command]
pub async fn get_device_sync_policy(
    state: tauri::State<'_, NodeState>,
) -> Result<DeviceSyncPolicy, CommandError> {
    Ok(state.node_handle().await?.device_sync_policy())
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
