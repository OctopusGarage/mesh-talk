//! Admitted IPC for shared group content. Private image overrides remain in avatars.
use super::{NodeState, parse_channel_id, parse_event_id};
use crate::{commands::CommandError, runtime_admission::RuntimeAdmission, state::AppState};
use mesh_talk_core::node::{ChannelMetadata, NodeRuntime};

fn check_account(runtime: &NodeRuntime, expected: &str) -> Result<(), CommandError> {
    if runtime.account_id() != expected {
        return Err(CommandError::Authorization("group account changed".into()));
    }
    Ok(())
}

#[tauri::command]
pub async fn channel_metadata(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    owner: String,
    expected_account: String,
    channel_id: String,
) -> Result<ChannelMetadata, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &state, &owner)?;
    admission
        .blocking(move |runtime, _| {
            check_account(runtime, &expected_account)?;
            runtime
                .handle()
                .channel_metadata(parse_channel_id(&channel_id)?)
                .map_err(CommandError::from)
        })
        .await
}

#[tauri::command]
pub async fn set_channel_cover(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    owner: String,
    expected_account: String,
    channel_id: String,
    content: Option<String>,
    expected_revision: Option<String>,
) -> Result<ChannelMetadata, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &state, &owner)?;
    save(
        admission,
        expected_account,
        channel_id,
        content,
        expected_revision,
        true,
    )
    .await
}

#[tauri::command]
pub async fn set_channel_announcement(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    owner: String,
    expected_account: String,
    channel_id: String,
    content: Option<String>,
    expected_revision: Option<String>,
) -> Result<ChannelMetadata, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &state, &owner)?;
    save(
        admission,
        expected_account,
        channel_id,
        content,
        expected_revision,
        false,
    )
    .await
}

async fn save(
    admission: RuntimeAdmission,
    expected_account: String,
    channel_id: String,
    content: Option<String>,
    expected_revision: Option<String>,
    cover: bool,
) -> Result<ChannelMetadata, CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let revision = expected_revision
        .as_deref()
        .map(parse_event_id)
        .transpose()?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                check_account(runtime, &expected_account)?;
                let node = runtime.handle();
                if cover {
                    node.set_channel_cover_if(channel, content.as_deref(), revision, |commit| {
                        admission.authorize_accept(runtime, commit)
                    })
                    .await?;
                } else {
                    node.set_channel_announcement_if(
                        channel,
                        content.as_deref(),
                        revision,
                        |commit| admission.authorize_accept(runtime, commit),
                    )
                    .await?;
                }
                let mut result = None;
                // Network fanout can outlive a desktop lock: recheck before releasing plaintext.
                admission.authorize_accept(runtime, &mut || {
                    result = Some(node.channel_metadata(channel)?);
                    Ok(())
                })?;
                result.ok_or_else(|| CommandError::Authorization("group session changed".into()))
            })
        })
        .await
}
