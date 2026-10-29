//! Exact-session adaptation of canonical forwarding operations.
use crate::{
    chat_commands::NodeState, commands::CommandError, runtime_admission::RuntimeAdmission,
    state::AppState,
};
use mesh_talk_core::node::{
    FavoriteSource, ForwardBatch, ForwardInspection, ForwardRequest, ForwardResult,
};
#[tauri::command]
pub async fn owner_inspect_forward_messages(
    owner: String,
    source: FavoriteSource,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<ForwardInspection, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .blocking(move |runtime, _| {
            runtime
                .handle()
                .inspect_forward_messages(&source)
                .map_err(CommandError::from)
        })
        .await
}
#[tauri::command]
pub async fn owner_list_forward_batches(
    owner: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<Vec<ForwardBatch>, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .blocking(move |runtime, _| Ok(runtime.handle().list_forward_batches()))
        .await
}
#[tauri::command]
pub async fn owner_forward_messages(
    owner: String,
    request: ForwardRequest,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<ForwardResult, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                runtime
                    .handle()
                    .forward_messages_if(request, |commit| {
                        admission.authorize_accept(runtime, commit)
                    })
                    .await
                    .map_err(CommandError::from)
            })
        })
        .await
}
#[tauri::command]
pub async fn owner_export_forwarded_attachment(
    owner: String,
    file_conv: String,
    index: usize,
    destination: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<String, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .blocking(move |runtime, _| {
            let bytes = runtime
                .handle()
                .forwarded_attachment_bytes(&file_conv, index)
                .map_err(CommandError::from)?;
            let path = std::path::Path::new(&destination);
            let parent = path
                .parent()
                .ok_or_else(|| CommandError::InvalidInput("Invalid destination".into()))?;
            let mut temporary = tempfile::NamedTempFile::new_in(parent)
                .map_err(|e| CommandError::Internal(e.to_string()))?;
            use std::io::Write;
            temporary
                .write_all(&bytes)
                .and_then(|_| temporary.as_file().sync_all())
                .map_err(|e| CommandError::Internal(e.to_string()))?;
            temporary
                .persist(path)
                .map_err(|e| CommandError::Internal(e.to_string()))?;
            Ok(destination)
        })
        .await
}
