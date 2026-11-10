//! Favorites IPC adapts the core collection under exact desktop-session admission.
use crate::{
    chat_commands::NodeState, commands::CommandError, runtime_admission::RuntimeAdmission,
    state::AppState,
};
use mesh_talk_core::node::{
    FavoriteMetadata, FavoriteSource, MessageFavorite, MessageFavoriteList,
};

#[tauri::command]
pub async fn owner_list_message_favorites(
    owner: String,
    query: String,
    offset: usize,
    limit: usize,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<MessageFavoriteList, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let response = admission
        .clone()
        .blocking(move |runtime, _| {
            let mut result = runtime
                .handle()
                .list_message_favorites(&query, offset, limit)
                .map_err(CommandError::from)?;
            // JSON escaping can expand a bounded binary record. Bound the actual
            // IPC representation too, and preserve continuation semantics.
            while serde_json::to_vec(&result)
                .map_err(|e| CommandError::Internal(e.to_string()))?
                .len()
                > 2 * 1024 * 1024
            {
                result.items.pop();
                result.has_more = true;
                if result.items.is_empty() {
                    return Err(CommandError::InvalidInput(
                        "favorite exceeds response byte limit".into(),
                    ));
                }
            }
            Ok(result)
        })
        .await?;
    admission.with_runtime(move |_| Ok(response)).await
}

#[tauri::command]
pub async fn owner_save_message_favorite(
    owner: String,
    source: FavoriteSource,
    metadata: FavoriteMetadata,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<MessageFavorite, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let response = admission
        .clone()
        .blocking(move |runtime, _| {
            // blocking() encloses this entire synchronous operation in matching
            // session/runtime authority. Do not acquire the same session lock twice.
            runtime
                .handle()
                .save_message_favorite_guarded(source, metadata, |commit| commit())
                .map_err(CommandError::from)
        })
        .await?;
    admission.with_runtime(move |_| Ok(response)).await
}

#[tauri::command]
pub async fn owner_edit_message_favorite(
    owner: String,
    id: String,
    expected_revision: u64,
    expected_device_id: String,
    metadata: FavoriteMetadata,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<MessageFavorite, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let response = admission
        .clone()
        .blocking(move |runtime, _| {
            runtime
                .handle()
                .edit_message_favorite_guarded(
                    &id,
                    expected_revision,
                    &expected_device_id,
                    metadata,
                    |commit| commit(),
                )
                .map_err(CommandError::from)
        })
        .await?;
    admission.with_runtime(move |_| Ok(response)).await
}

#[tauri::command]
pub async fn owner_remove_message_favorite(
    owner: String,
    id: String,
    expected_revision: u64,
    expected_device_id: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .blocking(move |runtime, _| {
            runtime
                .handle()
                .remove_message_favorite_guarded(
                    &id,
                    expected_revision,
                    &expected_device_id,
                    |commit| commit(),
                )
                .map_err(CommandError::from)
        })
        .await
}

#[tauri::command]
pub async fn owner_inspect_message_favorites(
    owner: String,
    source: FavoriteSource,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<mesh_talk_core::node::FavoriteInspection, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let result = admission
        .clone()
        .blocking(move |runtime, _| {
            runtime
                .handle()
                .inspect_message_favorites(&source)
                .map_err(CommandError::from)
        })
        .await?;
    admission.with_runtime(move |_| Ok(result)).await
}

#[tauri::command]
pub async fn owner_favorite_file_available(
    owner: String,
    file_conv: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<bool, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let id = crate::chat_commands::parse_channel_id(&file_conv)?;
    admission
        .with_runtime(move |runtime| Ok(runtime.handle().favorite_file_available(id)))
        .await
}

#[tauri::command]
pub async fn owner_read_favorite_file(
    owner: String,
    file_conv: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<tauri::ipc::Response, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let id = crate::chat_commands::parse_channel_id(&file_conv)?;
    let bytes = admission
        .clone()
        .blocking(move |runtime, _| {
            runtime
                .handle()
                .read_favorite_file(id)
                .ok_or_else(|| CommandError::InvalidInput("favorite file unavailable".into()))
        })
        .await?;
    admission
        .with_runtime(move |runtime| {
            if !runtime.handle().favorite_file_available(id) {
                return Err(CommandError::InvalidInput(
                    "favorite file unavailable".into(),
                ));
            }
            Ok(tauri::ipc::Response::new(bytes))
        })
        .await
}

#[tauri::command]
pub async fn owner_save_favorite_file(
    owner: String,
    file_conv: String,
    dest: String,
    to_directory: bool,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<String, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let id = crate::chat_commands::parse_channel_id(&file_conv)?;
    admission
        .blocking(move |runtime, _| {
            let node = runtime.handle();
            let path = std::path::Path::new(&dest);
            if to_directory {
                node.save_favorite_file_into_dir(id, path)
                    .map(|path| path.to_string_lossy().into_owned())
                    .map_err(CommandError::from)
            } else {
                node.save_favorite_file(id, path)
                    .map(|()| dest)
                    .map_err(CommandError::from)
            }
        })
        .await
}
