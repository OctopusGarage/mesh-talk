//! Session- and account-bound access to private synchronized documents.
use crate::{
    chat_commands::NodeState, commands::CommandError, runtime_admission::RuntimeAdmission,
    state::AppState,
};
use base64::Engine;
use mesh_talk_core::node::{
    AccountDocumentDomain, AccountDocumentKey, AccountDocumentMeta, AccountDocumentPage,
    DocumentConversation, DocumentVersion, NodeRuntime,
};

fn account(runtime: &NodeRuntime, expected: &str) -> Result<(), CommandError> {
    if runtime.account_id() != expected {
        return Err(CommandError::Authorization(
            "account changed during document operation".into(),
        ));
    }
    Ok(())
}

fn editable(key: &AccountDocumentKey) -> Result<(), CommandError> {
    if matches!(
        key,
        AccountDocumentKey::DmReaction { .. }
            | AccountDocumentKey::DmRecall { .. }
            | AccountDocumentKey::ChannelCustody { .. }
            | AccountDocumentKey::GroupInventory { .. }
    ) {
        return Err(CommandError::Authorization(
            "account controls require verified event ingestion".into(),
        ));
    }
    Ok(())
}

/// Only the explicit kept-source action attributes retained controls to this account.
#[tauri::command]
pub async fn publish_legacy_account_controls(
    owner: String,
    expected_account: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .blocking(move |runtime, _| {
            account(runtime, &expected_account)?;
            runtime
                .handle()
                .publish_legacy_account_controls()
                .map_err(CommandError::from)
        })
        .await
}

#[tauri::command]
pub async fn get_account_documents(
    owner: String,
    expected_account: String,
    domain: AccountDocumentDomain,
    after: Option<AccountDocumentKey>,
    limit: usize,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<AccountDocumentPage, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .with_runtime(move |runtime| {
            account(runtime, &expected_account)?;
            runtime
                .handle()
                .account_documents(domain, after.as_ref(), limit)
                .map_err(CommandError::from)
        })
        .await
}

#[tauri::command]
pub async fn get_account_document_meta(
    owner: String,
    expected_account: String,
    key: AccountDocumentKey,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<Option<AccountDocumentMeta>, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .with_runtime(move |runtime| {
            account(runtime, &expected_account)?;
            Ok(runtime.handle().account_document_meta(&key))
        })
        .await
}

#[tauri::command]
pub async fn read_account_document(
    owner: String,
    expected_account: String,
    key: AccountDocumentKey,
    expected: DocumentVersion,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<tauri::ipc::Response, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let read_account = expected_account.clone();
    let read_key = key.clone();
    let read_version = expected.clone();
    let bytes = admission
        .clone()
        .blocking(move |runtime, _| {
            account(runtime, &read_account)?;
            let document = runtime
                .handle()
                .account_document(&read_key)?
                .ok_or_else(|| CommandError::InvalidInput("document unavailable".into()))?;
            if document.meta.version() != read_version {
                return Err(CommandError::InvalidInput(
                    "document version changed".into(),
                ));
            }
            Ok(zeroize::Zeroizing::new(document.payload().to_vec()))
        })
        .await?;
    admission
        .with_runtime(move |runtime| {
            account(runtime, &expected_account)?;
            let current = runtime.handle().account_document_meta(&key);
            if current.is_none_or(|meta| meta.tombstone || meta.version() != expected) {
                return Err(CommandError::InvalidInput(
                    "document version changed".into(),
                ));
            }
            Ok(tauri::ipc::Response::new(bytes.to_vec()))
        })
        .await
}

#[tauri::command]
pub async fn put_account_document(
    owner: String,
    expected_account: String,
    key: AccountDocumentKey,
    payload_base64: String,
    expected: Option<DocumentVersion>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<AccountDocumentMeta, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    editable(&key)?;
    // Bound before allocating the decoded payload. Core enforces tighter domain limits.
    if payload_base64.len() > 16 * 1024 * 1024 {
        return Err(CommandError::Validation(
            "document exceeds the transfer limit".into(),
        ));
    }
    let payload = zeroize::Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .decode(payload_base64.as_bytes())
            .map_err(|_| CommandError::Validation("invalid document encoding".into()))?,
    );
    admission
        .blocking(move |runtime, _| {
            account(runtime, &expected_account)?;
            runtime
                .handle()
                .put_account_document_if(key, payload.to_vec(), expected, |commit| commit())
                .map_err(CommandError::from)
        })
        .await
}

#[tauri::command]
pub async fn remove_account_document(
    owner: String,
    expected_account: String,
    key: AccountDocumentKey,
    expected: Option<DocumentVersion>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<AccountDocumentMeta, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    editable(&key)?;
    admission
        .blocking(move |runtime, _| {
            account(runtime, &expected_account)?;
            runtime
                .handle()
                .remove_account_document_if(key, expected, |commit| commit())
                .map_err(CommandError::from)
        })
        .await
}

#[tauri::command]
pub async fn mark_account_message_seen(
    owner: String,
    expected_account: String,
    conversation: DocumentConversation,
    message_id: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<AccountDocumentMeta, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .blocking(move |runtime, _| {
            account(runtime, &expected_account)?;
            runtime
                .handle()
                .mark_account_message_seen_if(conversation, message_id, |commit| commit())
                .map_err(CommandError::from)
        })
        .await
}
