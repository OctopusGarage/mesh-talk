//! Owner-sensitive automatic delivery IPC. Legacy commands retain their shapes.

use crate::{
    chat_commands::{HistoryItem, NodeState},
    commands::CommandError,
    runtime_admission::RuntimeAdmission,
    state::AppState,
};
use mesh_talk_core::eventlog::EventId;
use serde::Serialize;

mod search;
pub use search::*;

fn parse_id(input: &str) -> Result<EventId, CommandError> {
    let bytes: [u8; 32] = hex::decode(input)
        .map_err(|_| CommandError::InvalidInput("malformed message id".into()))?
        .try_into()
        .map_err(|_| CommandError::InvalidInput("malformed message id".into()))?;
    Ok(EventId::new(bytes))
}

fn validate_account(account: &str) -> Result<(), CommandError> {
    if account.len() != 32 || hex::decode(account).is_err() {
        return Err(CommandError::InvalidInput("malformed account id".into()));
    }
    Ok(())
}

#[derive(Serialize)]
pub struct NodeIdentity {
    owner: String,
    display_name: String,
    device_id: String,
    account_id: String,
    history_revision: u64,
    supports_push_state: bool,
}

/// Atomically observes Session + runtime identity, including installation authority.
#[tauri::command]
pub async fn owner_node_identity(
    owner: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<NodeIdentity, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .with_runtime(|runtime| {
            Ok(NodeIdentity {
                owner,
                device_id: runtime.user_id().into(),
                account_id: runtime.account_id().into(),
                display_name: runtime.display_name(),
                history_revision: runtime.account_history_revision(),
                supports_push_state: true,
            })
        })
        .await
}

#[tauri::command]
pub async fn owner_enqueue_text(
    owner: String,
    account: String,
    text: String,
    reply_to: Option<String>,
    expires_at_ms: Option<u64>,
    protection: Option<mesh_talk_core::node::MessageProtection>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<String, CommandError> {
    let started = std::time::Instant::now();
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    validate_account(&account)?;
    let reply = reply_to.as_deref().map(parse_id).transpose()?;
    let result = admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let handle = runtime.handle();
                let id = if let Some(protection) = protection {
                    handle
                        .enqueue_protected_to_account_expiring_if(
                            &account,
                            text.as_bytes(),
                            reply,
                            expires_at_ms,
                            protection,
                            |accept| admission.authorize_accept(runtime, accept),
                        )
                        .await
                } else {
                    handle
                        .enqueue_to_account_expiring_if(
                            &account,
                            text.as_bytes(),
                            reply,
                            expires_at_ms,
                            |accept| admission.authorize_accept(runtime, accept),
                        )
                        .await
                }
                .map_err(CommandError::from)?;
                Ok(hex::encode(id.as_bytes()))
            })
        })
        .await;
    if started.elapsed() >= std::time::Duration::from_secs(1) {
        log::warn!(
            target: "mesh_talk::delivery",
            "slow local text acceptance elapsed_ms={} outcome={}",
            started.elapsed().as_millis(),
            if result.is_ok() { "accepted" } else { "rejected" },
        );
    }
    result
}

#[tauri::command]
pub async fn owner_enqueue_sticker(
    owner: String,
    account: String,
    sticker_id: String,
    fallback: String,
    expires_at_ms: Option<u64>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<String, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    validate_account(&account)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let id = runtime
                    .handle()
                    .enqueue_sticker_to_account_expiring_if(
                        &account,
                        &sticker_id,
                        fallback.as_bytes(),
                        expires_at_ms,
                        |accept| admission.authorize_accept(runtime, accept),
                    )
                    .await
                    .map_err(CommandError::from)?;
                Ok(hex::encode(id.as_bytes()))
            })
        })
        .await
}

/// Network fanout retains admission, but cannot hold the acceptance IPC open.
fn flush_channel_acceptance(
    admission: RuntimeAdmission,
    channel: mesh_talk_core::eventlog::ConversationId,
    id: EventId,
    file: Option<mesh_talk_core::eventlog::ConversationId>,
) {
    tauri::async_runtime::spawn(async move {
        let _ = admission
            .detached(move |runtime, _| {
                Box::pin(async move {
                    let node = runtime.handle();
                    if let Some(file) = file {
                        node.flush_channel_file(channel, file).await;
                    } else {
                        node.flush_accepted_message(
                            &mesh_talk_core::node::MessageDestination::Channel(channel),
                            id,
                        )
                        .await;
                    }
                    Ok(())
                })
            })
            .await;
    });
}

#[tauri::command]
pub async fn owner_enqueue_channel_text(
    owner: String,
    channel_id: String,
    text: String,
    reply_to: Option<String>,
    expires_at_ms: Option<u64>,
    protection: Option<mesh_talk_core::node::MessageProtection>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<String, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let channel = crate::chat_commands::parse_channel_id(&channel_id)?;
    let reply = reply_to.as_deref().map(parse_id).transpose()?;
    let flush = admission.clone();
    let id = admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let handle = runtime.handle();
                if let Some(protection) = protection {
                    handle.enqueue_protected_channel_message_reply_expiring_if(
                        channel,
                        text.as_bytes(),
                        reply,
                        expires_at_ms,
                        protection,
                        |accept| admission.authorize_accept(runtime, accept),
                    )
                } else {
                    handle.enqueue_channel_message_reply_expiring_if(
                        channel,
                        text.as_bytes(),
                        reply,
                        expires_at_ms,
                        |accept| admission.authorize_accept(runtime, accept),
                    )
                }
                .map_err(CommandError::from)
            })
        })
        .await?;
    flush_channel_acceptance(flush, channel, id, None);
    Ok(id.to_hex())
}

#[tauri::command]
pub async fn owner_enqueue_channel_sticker(
    owner: String,
    channel_id: String,
    sticker_id: String,
    fallback: String,
    expires_at_ms: Option<u64>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<String, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let channel = crate::chat_commands::parse_channel_id(&channel_id)?;
    let flush = admission.clone();
    let id = admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                runtime
                    .handle()
                    .enqueue_sticker_channel_expiring_if(
                        channel,
                        &sticker_id,
                        fallback.as_bytes(),
                        expires_at_ms,
                        |accept| admission.authorize_accept(runtime, accept),
                    )
                    .map_err(CommandError::from)
            })
        })
        .await?;
    flush_channel_acceptance(flush, channel, id, None);
    Ok(id.to_hex())
}

#[tauri::command]
pub async fn owner_enqueue_channel_file<R: tauri::Runtime>(
    owner: String,
    channel_id: String,
    path: String,
    media: bool,
    progress_key: Option<String>,
    expires_at_ms: Option<u64>,
    protection: Option<mesh_talk_core::node::MessageProtection>,
    handle: tauri::AppHandle<R>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<AcceptedFile, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let channel = crate::chat_commands::parse_channel_id(&channel_id)?;
    let flush = admission.clone();
    let cleanup = handle.clone();
    let mut progress =
        crate::events::ProgressThrottle::new(handle, progress_key.unwrap_or(channel_id), "prepare");
    let (id, file) = admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let kind = if media {
                    mesh_talk_core::file::FileKind::Media
                } else {
                    mesh_talk_core::file::FileKind::File
                };
                let handle = runtime.handle();
                let on_progress =
                    move |p: mesh_talk_core::node::FileProgress| progress.emit(p.done, p.total);
                let result = if let Some(protection) = protection {
                    handle
                        .enqueue_protected_file_channel_progress_expiring_if(
                            channel,
                            std::path::Path::new(&path),
                            kind,
                            on_progress,
                            expires_at_ms,
                            protection,
                            |accept| admission.authorize_accept(runtime, accept),
                        )
                        .await
                } else {
                    handle
                        .enqueue_file_channel_progress_expiring_if(
                            channel,
                            std::path::Path::new(&path),
                            kind,
                            on_progress,
                            expires_at_ms,
                            |accept| admission.authorize_accept(runtime, accept),
                        )
                        .await
                };
                let accepted = result.map_err(CommandError::from)?;
                crate::chat_commands::cleanup_pasted_file(&cleanup, &path);
                Ok(accepted)
            })
        })
        .await?;
    flush_channel_acceptance(flush, channel, id, Some(file));
    Ok(AcceptedFile {
        id: id.to_hex(),
        file_conv: hex::encode(file.as_bytes()),
    })
}

#[derive(Serialize)]
pub struct AcceptedFile {
    id: String,
    #[serde(rename = "fileConv")]
    file_conv: String,
}

#[tauri::command]
pub async fn owner_enqueue_file<R: tauri::Runtime>(
    owner: String,
    account: String,
    path: String,
    media: bool,
    progress_key: Option<String>,
    expires_at_ms: Option<u64>,
    protection: Option<mesh_talk_core::node::MessageProtection>,
    handle: tauri::AppHandle<R>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<AcceptedFile, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    validate_account(&account)?;
    let cleanup_handle = handle.clone();
    let mut progress = crate::events::ProgressThrottle::new(
        handle,
        progress_key.unwrap_or_else(|| account.clone()),
        "prepare",
    );
    #[cfg(test)]
    let progress_hook = app.owner_file_progress_hook.clone();
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let kind = if media {
                    mesh_talk_core::file::FileKind::Media
                } else {
                    mesh_talk_core::file::FileKind::File
                };
                let handle = runtime.handle();
                let on_progress = move |p: mesh_talk_core::node::FileProgress| {
                    progress.emit(p.done, p.total);
                    #[cfg(test)]
                    {
                        let hook = progress_hook.lock().unwrap().take();
                        if let Some(hook) = hook {
                            hook();
                        }
                    }
                };
                let result = if let Some(protection) = protection {
                    handle
                        .enqueue_protected_file_to_account_progress_expiring_if(
                            &account,
                            std::path::Path::new(&path),
                            kind,
                            on_progress,
                            expires_at_ms,
                            protection,
                            |accept| admission.authorize_accept(runtime, accept),
                        )
                        .await
                } else {
                    handle
                        .enqueue_file_to_account_progress_expiring_if(
                            &account,
                            std::path::Path::new(&path),
                            kind,
                            on_progress,
                            expires_at_ms,
                            |accept| admission.authorize_accept(runtime, accept),
                        )
                        .await
                };
                let (id, conv) = result.map_err(CommandError::from)?;
                crate::chat_commands::cleanup_pasted_file(&cleanup_handle, &path);
                Ok(AcceptedFile {
                    id: hex::encode(id.as_bytes()),
                    file_conv: hex::encode(conv.as_bytes()),
                })
            })
        })
        .await
}

#[tauri::command]
pub async fn owner_account_history<R: tauri::Runtime>(
    handle: tauri::AppHandle<R>,
    owner: String,
    account: String,
    limit: usize,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<Vec<HistoryItem>, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    validate_account(&account)?;
    let history = admission
        .with_runtime(|runtime| {
            Ok(runtime
                .account_history(&account, limit.min(500))
                .into_iter()
                .map(HistoryItem::from)
                .collect())
        })
        .await?;
    crate::app_lock::reject_if_locked(&handle)?;
    Ok(history)
}

#[derive(Serialize)]
pub struct MessageStatus {
    id: String,
    status: mesh_talk_core::node::DeliveryStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    observation: Option<mesh_talk_core::node::DeliveryObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_progress: Option<OutgoingFileStatus>,
}

#[derive(Serialize)]
pub struct OutgoingFileStatus {
    done: u32,
    total: u32,
    delivered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    consent: Option<&'static str>,
}

#[tauri::command]
pub async fn owner_delivery_statuses(
    owner: String,
    account: String,
    ids: Vec<String>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<Vec<MessageStatus>, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    validate_account(&account)?;
    if ids.len() > 256 {
        return Err(CommandError::InvalidInput(
            "at most 256 message ids are allowed".into(),
        ));
    }
    let ids = ids
        .iter()
        .map(|id| parse_id(id))
        .collect::<Result<Vec<_>, _>>()?;
    admission
        .with_runtime(|runtime| {
            Ok(runtime
                .handle()
                .account_delivery_observations(&account, &ids)
                .into_iter()
                .map(|(id, status, observation)| MessageStatus {
                    id: hex::encode(id.as_bytes()),
                    status,
                    observation,
                    file_progress: runtime.handle().outgoing_file_progress(id).map(|progress| {
                        OutgoingFileStatus {
                            done: progress.done,
                            total: progress.total,
                            delivered: progress.delivered,
                            consent: progress.consent.map(|decision| match decision {
                                mesh_talk_core::node::ReceiveDecision::Pending => "pending",
                                mesh_talk_core::node::ReceiveDecision::Declined => "declined",
                                mesh_talk_core::node::ReceiveDecision::Accepted => "accepted",
                            }),
                        }
                    }),
                })
                .collect())
        })
        .await
}

#[cfg(test)]
mod tests {
    use crate::{
        chat_commands::NodeState,
        services::{auth_service::AuthService, user::User},
        state::AppState,
    };
    use mesh_talk_core::{identity::manager::IdentityManager, storage::file_manager::FileManager};
    use std::sync::Arc;

    fn user(owner: &str) -> User {
        User {
            user_id: owner.into(),
            account_login_name: None,
            name: owner.into(),
            display_name: owner.into(),
            address: "fixture".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        }
    }

    fn reserve_discovery_port() -> tokio::net::UdpSocket {
        // Match runtime discovery's reuse options and wildcard address, retaining
        // ownership through startup rather than returning an unreserved port.
        tauri::async_runtime::block_on(async {
            mesh_talk_core::transport::net::discovery_socket(0).unwrap()
        })
    }

    #[test]
    fn discovery_reservation_survives_competing_nonreuse_bind_until_runtime_start() {
        let root = tempfile::tempdir().unwrap();
        let reservation = reserve_discovery_port();
        let port = reservation.local_addr().unwrap().port();
        let competitor = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, port));
        let competitor_blocked = competitor.is_err();
        let started = tauri::async_runtime::block_on(mesh_talk_core::node::NodeRuntime::start(
            root.path(),
            "reserved",
            "Reserved",
            "pw",
            port,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        ));
        drop(reservation);
        drop(competitor);
        let startup_error = started.as_ref().err().map(ToString::to_string);
        if let Ok(runtime) = started {
            tauri::async_runtime::block_on(runtime.stop());
        }
        assert!(
            competitor_blocked,
            "non-reuse competitor acquired reserved port; real startup error: {startup_error:?}"
        );
        assert!(
            startup_error.is_none(),
            "compatible reservation prevented runtime startup: {startup_error:?}"
        );
    }

    struct Fixture {
        root: tempfile::TempDir,
        app_state: AppState,
        node: NodeState,
        webview: tauri::WebviewWindow<tauri::test::MockRuntime>,
        discovery_port: u16,
        // Held-state probes must not consume Tauri's shared IPC worker threads.
        // Kept alive until Drop has joined the actual node's retirement barrier.
        _background: Option<tokio::runtime::Runtime>,
    }

    impl Fixture {
        fn new() -> Self {
            Self::create(false)
        }

        fn create(authenticated: bool) -> Self {
            // Isolate background producers from deliberately stalled legacy
            // privacy callbacks, including regressions that retain a state writer,
            // so low-core runners can still execute registered logout.
            let background = authenticated.then(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .unwrap()
            });
            let password = if authenticated {
                "password-owner"
            } else {
                "pw"
            };
            let root = tempfile::tempdir().unwrap();
            let app_state = AppState::new(AuthService::new(Arc::new(IdentityManager::new(
                FileManager::new(root.path().to_owned()),
            ))));
            let owner = if authenticated {
                app_state
                    .auth_service()
                    .register("alice".into(), password.into(), "fixture".into())
                    .unwrap();
                let (user, _) = app_state
                    .auth_service()
                    .login("alice".into(), password.into())
                    .unwrap();
                user.user_id.clone()
            } else {
                app_state
                    .session()
                    .set("a".into(), user("alice"), "pw".into());
                "alice".into()
            };
            let node = NodeState::empty();
            let app = tauri::test::mock_builder()
                .manage(app_state.clone())
                .manage(app_state.app_lock().clone())
                .manage(node.clone())
                .manage(crate::trust::TrustState::default())
                .manage(crate::avatars::AvatarsState::isolated(
                    root.path().to_owned(),
                ))
                .manage(crate::settings::SettingsState::isolated(
                    root.path().join("settings.json"),
                ))
                .invoke_handler(tauri::generate_handler![
                    crate::chat_commands::search,
                    crate::chat_commands::channel_history,
                    crate::chat_commands::read_file,
                    crate::chat_commands::read_media,
                    crate::trust::mark_verified,
                    crate::trust::get_trust,
                    crate::avatars::get_avatars,
                    crate::avatars::set_avatar,
                    crate::avatars::publish_legacy_avatar_overrides,
                    crate::drafts::load_conversation_drafts,
                    crate::drafts::save_conversation_drafts,
                    crate::drafts::publish_legacy_conversation_drafts,
                    crate::account_documents::get_account_documents,
                    crate::account_documents::get_account_document_meta,
                    crate::account_documents::read_account_document,
                    crate::account_documents::put_account_document,
                    crate::account_documents::remove_account_document,
                    crate::account_documents::mark_account_message_seen,
                    crate::account_documents::publish_legacy_account_controls,
                    crate::account_profile::get_account_profile,
                    crate::account_profile::save_account_bio,
                    crate::chat_commands::send_file_to_account,
                    crate::chat_commands::send_to_account,
                    crate::chat_commands::send_sticker,
                    crate::chat_commands::create_channel,
                    crate::chat_commands::channel_history,
                    crate::chat_commands::rescan_peers,
                    crate::chat_commands::probe_peer_endpoint,
                    crate::chat_commands::rename_channel,
                    crate::chat_commands::set_channel_replica_enabled,
                    crate::chat_commands::set_channel_history_policy,
                    crate::chat_commands::channel_members,
                    crate::chat_commands::add_channel_member,
                    crate::chat_commands::remove_channel_member,
                    crate::chat_commands::remove_channel_account_members,
                    crate::chat_commands::transfer_channel_owner,
                    crate::chat_commands::leave_channel,
                    crate::chat_commands::accept_channel_invite,
                    crate::chat_commands::dissolve_channel,
                    crate::chat_commands::send_channel_message,
                    crate::chat_commands::react_channel,
                    crate::chat_commands::send_dm,
                    crate::chat_commands::send_call_signal,
                    crate::chat_commands::react_dm,
                    crate::chat_commands::react_account,
                    crate::chat_commands::delete_message,
                    crate::chat_commands::recall_message,
                    crate::chat_commands::clear_conversation,
                    crate::chat_commands::start_linking,
                    crate::chat_commands::stop_linking,
                    crate::chat_commands::link_device,
                    crate::chat_commands::rekey_account,
                    crate::chat_commands::publish_avatar,
                    crate::chat_commands::set_attachment_global_auto,
                    crate::chat_commands::set_attachment_contact_auto,
                    crate::chat_commands::decide_attachment,
                    crate::chat_commands::send_file_dm,
                    crate::chat_commands::send_file_channel,
                    crate::chat_commands::save_file,
                    crate::chat_commands::save_file_to_dir,
                    crate::chat_commands::write_temp_file,
                    super::owner_search_recent,
                    super::owner_search_context,
                    super::owner_reveal_message,
                    super::owner_reveal_image,
                    crate::message_favorites::owner_list_message_favorites,
                    crate::message_favorites::owner_save_message_favorite,
                    crate::message_favorites::owner_edit_message_favorite,
                    crate::message_favorites::owner_remove_message_favorite,
                    crate::message_favorites::owner_inspect_message_favorites,
                    crate::message_favorites::owner_favorite_file_available,
                    crate::message_favorites::owner_read_favorite_file,
                    crate::message_favorites::owner_save_favorite_file,
                    super::owner_node_identity,
                    super::owner_enqueue_text,
                    super::owner_enqueue_channel_text,
                    super::owner_enqueue_channel_sticker,
                    super::owner_enqueue_channel_file,
                    super::owner_enqueue_sticker,
                    super::owner_enqueue_file,
                    super::owner_account_history,
                    super::owner_delivery_statuses,
                    crate::chat_commands::get_device_sync_policy,
                    crate::chat_commands::get_device_sync_status,
                    crate::chat_commands::set_device_sync_category,
                    crate::chat_commands::get_account_preferences,
                    crate::chat_commands::set_account_preference,
                    crate::commands::logout,
                    crate::privacy_commands::get_privacy,
                    crate::privacy_commands::get_account_binding_changes,
                    crate::privacy_commands::accept_account_binding_change,
                    crate::privacy_commands::set_invisible,
                    crate::privacy_commands::set_privacy_allowed,
                    crate::privacy_commands::initiate_privacy_contact
                ])
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap();
            let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
                .build()
                .unwrap();
            let reservation = reserve_discovery_port();
            let discovery_port = reservation.local_addr().unwrap().port();
            let startup = mesh_talk_core::node::NodeRuntime::start(
                root.path(),
                &owner,
                "Alice",
                password,
                discovery_port,
                |_| {},
                |_| {},
                |_| {},
                |_| {},
                |_| {},
            );
            let runtime = match background.as_ref() {
                Some(background) => background.block_on(startup),
                None => tauri::async_runtime::block_on(startup),
            }
            .unwrap();
            drop(reservation);
            *node.0.blocking_write() = Some(runtime);
            Self {
                root,
                app_state,
                node,
                webview,
                discovery_port,
                _background: background,
            }
        }

        fn invoke(
            &self,
            command: &str,
            body: serde_json::Value,
        ) -> Result<serde_json::Value, serde_json::Value> {
            invoke(&self.webview, command, body)
        }

        fn offline_peer(&self) -> String {
            use mesh_talk_core::{
                discovery::Announce,
                identity::{account::Account, device::DeviceIdentity},
            };
            let identity = DeviceIdentity::generate();
            let account = Account::generate();
            self.announce_peer(Announce::new_with_account(
                &identity,
                &account,
                "Offline target",
                1,
            ));
            account.account_id()
        }

        fn sync_documents(&self) -> [Vec<u8>; 2] {
            let guard = self.node.0.blocking_write();
            let runtime = guard.as_ref().unwrap();
            let directory = self
                .root
                .path()
                .join("accounts")
                .join(runtime.host_account_id().unwrap());
            [
                format!("account-sync-policy-{}.log", runtime.account_id()),
                "account-preferences.log".into(),
            ]
            .map(|name| std::fs::read(directory.join(name)).unwrap())
        }

        fn announce_peer(&self, announce: mesh_talk_core::discovery::Announce) {
            let uid = announce.public().user_id();
            // Signed discovery is real; this fixture deliberately has no accepting
            // remote node, so the automatic worker cannot produce a receipt.
            let bytes = mesh_talk_core::discovery::announce::encode(&announce);
            let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            socket
                .send_to(&bytes, (std::net::Ipv4Addr::LOCALHOST, self.discovery_port))
                .unwrap();
            tauri::async_runtime::block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    while self
                        .node
                        .0
                        .write()
                        .await
                        .as_ref()
                        .unwrap()
                        .peer_public(&uid)
                        .is_none()
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            });
        }
    }

    #[test]
    fn registered_search_recent_and_exact_context_shapes() {
        let fixture = Fixture::new();
        let account = fixture.offline_peer();
        let id = fixture
            .invoke(
                "owner_enqueue_text",
                serde_json::json!({"owner":"alice", "account":account, "text":"exact token"}),
            )
            .unwrap();
        let results = fixture
            .invoke(
                "owner_search_recent",
                serde_json::json!({"owner":"alice", "query":"exact token"}),
            )
            .unwrap();
        assert_eq!(
            results["coverage"],
            serde_json::json!({"scope":"recent-window","per_conversation_limit":2000,"result_limit":200,"truncated":false})
        );
        assert_eq!(results["hits"].as_array().unwrap().len(), 1);
        let reference = serde_json::json!({"kind":"account", "target":account, "message_id":id});
        assert_eq!(results["hits"][0]["reference"], reference);
        assert_eq!(results["hits"][0]["text"], "exact token");
        let context = fixture
            .invoke(
                "owner_search_context",
                serde_json::json!({"owner":"alice", "reference":reference}),
            )
            .unwrap();
        assert_eq!(context["status"], "found");
        assert_eq!(context["newer_entries"], 0);
        assert_eq!(results["hits"][0]["expires_at_ms"], serde_json::Value::Null);
        assert_eq!(context["reference"], reference);
        assert_eq!(context["messages"].as_array().unwrap().len(), 1);
        assert_eq!(context["messages"][0]["id"], id);
        for message_id in [
            serde_json::Value::Null,
            serde_json::json!("00".repeat(32)),
            serde_json::json!("ff".repeat(32)),
        ] {
            let reference =
                serde_json::json!({"kind":"account", "target":account,"message_id":message_id});
            assert_eq!(
                fixture
                    .invoke(
                        "owner_search_context",
                        serde_json::json!({"owner":"alice","reference":reference})
                    )
                    .unwrap(),
                serde_json::json!({"status":"unavailable","reference":reference,"messages":[],"newer_entries":null})
            );
        }
        let channel = fixture
            .invoke(
                "create_channel",
                serde_json::json!({"name":"Search channel", "memberIds":[]}),
            )
            .unwrap();
        fixture
            .invoke(
                "send_channel_message",
                serde_json::json!({"channelId":channel,"text":"channel token"}),
            )
            .unwrap();
        let results = fixture
            .invoke(
                "owner_search_recent",
                serde_json::json!({"owner":"alice","query":"channel token"}),
            )
            .unwrap();
        assert_eq!(results["hits"][0]["reference"]["kind"], "channel");
        let context = fixture
            .invoke(
                "owner_search_context",
                serde_json::json!({"owner":"alice","reference":results["hits"][0]["reference"]}),
            )
            .unwrap();
        assert_eq!(context["status"], "found");
    }

    fn message_favorite_command_cases(owner: &str) -> Vec<(&'static str, serde_json::Value)> {
        vec![
            (
                "owner_inspect_message_favorites",
                serde_json::json!({"owner":owner,"source":{"kind":"account","target":"01".repeat(16),"message_ids":["01".repeat(32)]}}),
            ),
            (
                "owner_favorite_file_available",
                serde_json::json!({"owner":owner,"fileConv":"01".repeat(32)}),
            ),
            (
                "owner_read_favorite_file",
                serde_json::json!({"owner":owner,"fileConv":"01".repeat(32)}),
            ),
            (
                "owner_save_favorite_file",
                serde_json::json!({"owner":owner,"fileConv":"01".repeat(32),"dest":"/unused-test-output","toDirectory":false}),
            ),
            (
                "owner_list_message_favorites",
                serde_json::json!({"owner":owner,"query":"","offset":0,"limit":20}),
            ),
            (
                "owner_save_message_favorite",
                serde_json::json!({"owner":owner,"source":{"kind":"account","target":"01".repeat(16),"message_ids":["01".repeat(32)]},"metadata":{"title":"","description":"","notes":{}}}),
            ),
            (
                "owner_edit_message_favorite",
                serde_json::json!({"owner":owner,"id":"01".repeat(32),"expectedRevision":1,"expectedDeviceId":"01".repeat(16),"metadata":{"title":"","description":"","notes":{}}}),
            ),
            (
                "owner_remove_message_favorite",
                serde_json::json!({"owner":owner,"id":"01".repeat(32),"expectedRevision":1,"expectedDeviceId":"01".repeat(16)}),
            ),
        ]
    }

    #[test]
    fn registered_account_documents_preserve_account_scope_cas_and_queued_owner() {
        let fixture = Fixture::new();
        let account = fixture
            .node
            .0
            .blocking_read()
            .as_ref()
            .unwrap()
            .account_id()
            .to_owned();
        let key = serde_json::json!({"kind":"draft","conversation":{"kind":"account","id":"11".repeat(16)}});
        let body = serde_json::json!({"owner":"alice","expectedAccount":account,"key":key,"payloadBase64":"aGVsbG8=","expected":null});
        let first = fixture
            .invoke("put_account_document", body.clone())
            .unwrap();
        assert_eq!(first["content_len"], 5);
        assert!(
            fixture
                .invoke("put_account_document", body.clone())
                .is_err(),
            "create-only must preserve an existing version"
        );
        let mut other = body.clone();
        other["expectedAccount"] = serde_json::json!("22".repeat(16));
        assert_eq!(
            fixture.invoke("put_account_document", other).unwrap_err()["kind"],
            "authorization"
        );
        let meta = fixture
            .invoke(
                "get_account_document_meta",
                serde_json::json!({"owner":"alice","expectedAccount":account,"key":key}),
            )
            .unwrap();
        assert_eq!(meta, first);
        let (entered, captured) = std::sync::mpsc::channel();
        *fixture.app_state.owner_command_captured.lock().unwrap() = Some(entered);
        let lifecycle = fixture.node.0.blocking_write();
        let webview = fixture.webview.clone();
        let pending = std::thread::spawn(move || invoke(&webview, "put_account_document", body));
        captured
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        fixture
            .app_state
            .session()
            .set("replacement".into(), user("alice"), "pw".into());
        drop(lifecycle);
        assert_eq!(
            pending.join().unwrap().unwrap_err()["kind"],
            "authorization"
        );
        *fixture.app_state.owner_command_captured.lock().unwrap() = None;
    }

    #[test]
    fn registered_account_controls_reject_generic_writes_and_removals() {
        let fixture = Fixture::new();
        let account = fixture
            .node
            .0
            .blocking_read()
            .as_ref()
            .unwrap()
            .account_id()
            .to_owned();
        for kind in ["dm_reaction", "dm_recall"] {
            let mut key = serde_json::json!({"kind":kind,"peer_account":"11".repeat(16),"target":"22".repeat(32),"actor_account":account});
            if kind == "dm_reaction" {
                key["emoji"] = serde_json::json!("👍");
            }
            for command in ["put_account_document", "remove_account_document"] {
                let result = fixture.invoke(command,serde_json::json!({"owner":"alice","expectedAccount":account,"key":key,"payloadBase64":"e30=","expected":null}));
                assert_eq!(
                    result.unwrap_err()["kind"],
                    "authorization",
                    "generic IPC must not forge a control: {command} {kind}"
                );
            }
            assert!(
                fixture
                    .invoke(
                        "get_account_document_meta",
                        serde_json::json!({"owner":"alice","expectedAccount":account,"key":key})
                    )
                    .unwrap()
                    .is_null()
            );
        }
    }

    #[test]
    fn registered_account_control_source_import_checks_account_and_queued_session() {
        let fixture = Fixture::new();
        let account = fixture
            .node
            .0
            .blocking_read()
            .as_ref()
            .unwrap()
            .account_id()
            .to_owned();
        assert_eq!(
            fixture
                .invoke(
                    "publish_legacy_account_controls",
                    serde_json::json!({"owner":"alice","expectedAccount":"11".repeat(16)})
                )
                .unwrap_err()["kind"],
            "authorization"
        );
        let body = serde_json::json!({"owner":"alice","expectedAccount":account});
        fixture
            .invoke("publish_legacy_account_controls", body.clone())
            .unwrap();
        let (entered, captured) = std::sync::mpsc::channel();
        *fixture.app_state.owner_command_captured.lock().unwrap() = Some(entered);
        let lifecycle = fixture.node.0.blocking_write();
        let webview = fixture.webview.clone();
        let pending =
            std::thread::spawn(move || invoke(&webview, "publish_legacy_account_controls", body));
        captured
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        fixture
            .app_state
            .session()
            .set("replacement".into(), user("alice"), "pw".into());
        drop(lifecycle);
        assert_eq!(
            pending.join().unwrap().unwrap_err()["kind"],
            "authorization"
        );
        *fixture.app_state.owner_command_captured.lock().unwrap() = None;
    }

    #[test]
    fn registered_public_avatar_rejects_originating_wrong_account() {
        let fixture = Fixture::new();
        let result = fixture.invoke(
            "publish_avatar",
            serde_json::json!({"owner":"alice","expectedAccount":"22".repeat(16),"avatar":null}),
        );
        assert!(
            result.is_err(),
            "a former account image must never be published under the replacement root"
        );
    }

    #[test]
    fn registered_public_avatar_rejects_queued_same_owner_account_replacement() {
        let fixture = Fixture::new();
        let replacement = Fixture::new();
        let account = fixture
            .node
            .0
            .blocking_read()
            .as_ref()
            .unwrap()
            .account_id()
            .to_owned();
        let replacement_account = replacement
            .node
            .0
            .blocking_read()
            .as_ref()
            .unwrap()
            .account_id()
            .to_owned();
        assert_ne!(account, replacement_account);
        let (entered, captured) = std::sync::mpsc::channel();
        *fixture.app_state.owner_command_captured.lock().unwrap() = Some(entered);
        let mut lifecycle = fixture.node.0.blocking_write();
        let webview = fixture.webview.clone();
        let pending = std::thread::spawn(move || {
            invoke(
                &webview,
                "publish_avatar",
                serde_json::json!({"owner":"alice","expectedAccount":account,"avatar":null}),
            )
        });
        captured
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let old = std::mem::replace(&mut *lifecycle, replacement.node.0.blocking_write().take());
        drop(lifecycle);
        assert!(
            pending.join().unwrap().is_err(),
            "queued old-root image cannot publish into replacement account with the same local owner"
        );
        *fixture.app_state.owner_command_captured.lock().unwrap() = None;
        tauri::async_runtime::block_on(async {
            old.unwrap().stop().await;
        });
    }

    #[test]
    fn registered_shared_drafts_reject_wrong_account_writer_and_queued_session() {
        let fixture = Fixture::new();
        let account = fixture
            .node
            .0
            .blocking_read()
            .as_ref()
            .unwrap()
            .account_id()
            .to_owned();
        let load = serde_json::json!({"owner":"alice","expectedAccount":account});
        let snapshot = fixture
            .invoke("load_conversation_drafts", load.clone())
            .unwrap();
        let conversation = format!("account:{}", "11".repeat(16));
        let mut changes = serde_json::Map::new();
        changes.insert(
            conversation.clone(),
            serde_json::json!({"text":"unfinished shared draft","protection":null,"expected":null}),
        );
        let body = serde_json::json!({"owner":"alice","expectedAccount":account,"session":snapshot["session"],"drafts":{},"protections":{},"changes":changes});
        let saved = fixture
            .invoke("save_conversation_drafts", body.clone())
            .unwrap();
        assert_eq!(saved["drafts"][&conversation], "unfinished shared draft");
        let mut wrong = load;
        wrong["expectedAccount"] = serde_json::json!("22".repeat(16));
        assert_eq!(
            fixture
                .invoke("load_conversation_drafts", wrong)
                .unwrap_err()["kind"],
            "authorization"
        );
        let mut stale = body.clone();
        stale["session"] = serde_json::json!("retired-writer");
        assert!(fixture.invoke("save_conversation_drafts", stale).is_err());
        let (entered, captured) = std::sync::mpsc::channel();
        *fixture.app_state.owner_command_captured.lock().unwrap() = Some(entered);
        let lifecycle = fixture.node.0.blocking_write();
        let webview = fixture.webview.clone();
        let pending =
            std::thread::spawn(move || invoke(&webview, "save_conversation_drafts", body));
        captured
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        fixture
            .app_state
            .session()
            .set("replacement".into(), user("alice"), "pw".into());
        drop(lifecycle);
        assert_eq!(
            pending.join().unwrap().unwrap_err()["kind"],
            "authorization"
        );
        *fixture.app_state.owner_command_captured.lock().unwrap() = None;
    }

    #[test]
    fn registered_avatar_overrides_keep_tombstones_and_refuse_other_account_writes() {
        let fixture = Fixture::new();
        let account = fixture
            .node
            .0
            .blocking_read()
            .as_ref()
            .unwrap()
            .account_id()
            .to_owned();
        let target = "11".repeat(16);
        let image = "data:image/png;base64,iVBORw0KGgo=";
        let legacy = fixture.root.path().join("accounts/alice/avatars.json");
        std::fs::write(
            &legacy,
            serde_json::to_vec(&serde_json::json!({"avatars":{target.clone():image}})).unwrap(),
        )
        .unwrap();
        assert!(
            fixture
                .invoke(
                    "get_avatars",
                    serde_json::json!({"owner":"alice","expectedAccount":account})
                )
                .unwrap()
                .get(&target)
                .is_none(),
            "unbound legacy photos cannot be attributed by a one-device registry"
        );
        assert!(
            fixture
                .invoke("get_avatars", serde_json::json!({"owner":"alice"}))
                .unwrap()
                .get(&target)
                .is_none(),
            "bootstrap cannot expose another account's legacy photos"
        );
        let set = serde_json::json!({"owner":"alice","expectedAccount":account,"id":target,"dataUrl":image});
        fixture.invoke("set_avatar", set.clone()).unwrap();
        let get = serde_json::json!({"owner":"alice","expectedAccount":account});
        assert_eq!(
            fixture.invoke("get_avatars", get.clone()).unwrap()[&target],
            image
        );
        let mut wrong = set.clone();
        wrong["expectedAccount"] = serde_json::json!("22".repeat(16));
        assert_eq!(
            fixture.invoke("set_avatar", wrong).unwrap_err()["kind"],
            "authorization"
        );
        let mut clear = set;
        clear["dataUrl"] = serde_json::Value::Null;
        fixture.invoke("set_avatar", clear).unwrap();
        assert!(
            fixture
                .invoke("get_avatars", get.clone())
                .unwrap()
                .get(&target)
                .is_none()
        );
        let legacy = fixture.root.path().join("accounts/alice/avatars.json");
        std::fs::write(
            &legacy,
            serde_json::to_vec(&serde_json::json!({"avatars":{target.clone():image}})).unwrap(),
        )
        .unwrap();
        fixture
            .invoke("publish_legacy_avatar_overrides", get.clone())
            .unwrap();
        assert!(
            fixture
                .invoke("get_avatars", get)
                .unwrap()
                .get(&target)
                .is_none(),
            "source seeding must not resurrect a deleted picture"
        );
    }

    #[test]
    fn registered_message_favorites_validate_owner_and_reject_queued_session_changes() {
        use tauri::Manager;
        let fixture = Fixture::new();
        for (command, body) in message_favorite_command_cases("bob") {
            assert_eq!(
                fixture.invoke(command, body).unwrap_err()["kind"],
                "authorization"
            );
        }
        for change in ["relogin", "logout", "lock"] {
            for (command, body) in message_favorite_command_cases("alice") {
                fixture.app_state.app_lock().clear();
                fixture
                    .app_state
                    .session()
                    .set("entry".into(), user("alice"), "pw".into());
                let (entered, captured) = std::sync::mpsc::channel();
                *fixture.app_state.owner_command_captured.lock().unwrap() = Some(entered);
                let lifecycle = fixture.node.0.blocking_write();
                let webview = fixture.webview.clone();
                let pending = std::thread::spawn(move || invoke(&webview, command, body));
                let captured = captured.recv_timeout(std::time::Duration::from_secs(2));
                match change {
                    "logout" => fixture.app_state.session().clear(),
                    "relogin" => fixture.app_state.session().set(
                        "replacement".into(),
                        user("alice"),
                        "pw".into(),
                    ),
                    _ => crate::app_lock::lock_app(
                        fixture.webview.state::<AppState>(),
                        fixture.webview.state::<crate::app_lock::AppLock>(),
                    )
                    .unwrap(),
                }
                drop(lifecycle);
                let result = pending.join().unwrap();
                assert!(captured.is_ok(), "{command} captures entry before waiting");
                assert_eq!(
                    result.unwrap_err()["kind"],
                    "authorization",
                    "{command}: {change}"
                );
                *fixture.app_state.owner_command_captured.lock().unwrap() = None;
            }
        }
    }

    #[test]
    fn registered_message_favorites_save_search_edit_remove_and_keep_original() {
        let fixture = Fixture::new();
        let target = fixture.offline_peer();
        let sent = fixture.invoke("owner_enqueue_text", serde_json::json!({"owner":"alice","account":target,"text":"useful original","replyTo":null})).unwrap();
        let item = fixture.invoke("owner_save_message_favorite", serde_json::json!({"owner":"alice","source":{"kind":"account","target":target,"message_ids":[sent]},"metadata":{"title":"Runbook","description":"deployment steps","notes":{}}})).unwrap();
        let list = fixture
            .invoke(
                "owner_list_message_favorites",
                serde_json::json!({"owner":"alice","query":"deployment","offset":0,"limit":20}),
            )
            .unwrap();
        assert_eq!(list["items"][0]["id"], item["id"]);
        let edited = fixture.invoke("owner_edit_message_favorite", serde_json::json!({"owner":"alice","id":item["id"],"expectedRevision":item["revision"],"expectedDeviceId":item["device_id"],"metadata":{"title":"Updated","description":"new note","notes":{}}})).unwrap();
        assert!(fixture.invoke("owner_remove_message_favorite", serde_json::json!({"owner":"alice","id":item["id"],"expectedRevision":item["revision"],"expectedDeviceId":item["device_id"]})).is_err());
        fixture.invoke("owner_remove_message_favorite", serde_json::json!({"owner":"alice","id":edited["id"],"expectedRevision":edited["revision"],"expectedDeviceId":edited["device_id"]})).unwrap();
        let list = fixture
            .invoke(
                "owner_list_message_favorites",
                serde_json::json!({"owner":"alice","query":"","offset":0,"limit":20}),
            )
            .unwrap();
        assert_eq!(list["items"], serde_json::json!([]));
        let history = fixture
            .invoke(
                "owner_account_history",
                serde_json::json!({"owner":"alice","account":target,"limit":20}),
            )
            .unwrap();
        assert_eq!(history[0]["text"], "useful original");
    }

    #[test]
    fn registered_message_favorite_files_retain_export_and_revoke_access() {
        use tauri::Manager;

        let fixture = Fixture::new();
        let target = fixture.offline_peer();
        let bytes = b"retained attachment\0\xff\n";
        let source_path = fixture.root.path().join("attachment.bin");
        std::fs::write(&source_path, bytes).unwrap();
        let file = fixture
            .invoke(
                "owner_enqueue_file",
                serde_json::json!({"owner":"alice","account":target,"path":source_path,"media":false}),
            )
            .unwrap();
        let file_conv = &file["fileConv"];
        let available = serde_json::json!({"owner":"alice","fileConv":file_conv});
        let export_path = fixture.root.path().join("export.bin");
        let export = serde_json::json!({"owner":"alice","fileConv":file_conv,"dest":export_path,"toDirectory":false});

        // Ordinary attachment availability alone never authorizes Favorites IPC.
        assert_eq!(
            fixture
                .invoke("owner_favorite_file_available", available.clone())
                .unwrap(),
            false
        );
        assert!(
            fixture
                .invoke("owner_read_favorite_file", available.clone())
                .is_err()
        );
        assert!(
            fixture
                .invoke("owner_save_favorite_file", export.clone())
                .is_err()
        );
        assert!(!export_path.exists());

        let favorite = fixture.invoke("owner_save_message_favorite", serde_json::json!({"owner":"alice","source":{"kind":"account","target":target,"message_ids":[file["id"]]},"metadata":{"title":"Attachment","description":"","notes":{}}})).unwrap();
        std::fs::remove_file(&source_path).unwrap();
        fixture
            .invoke(
                "clear_conversation",
                serde_json::json!({"convId":target,"isChannel":false}),
            )
            .unwrap();
        assert_eq!(
            fixture
                .invoke(
                    "owner_account_history",
                    serde_json::json!({"owner":"alice","account":target,"limit":20})
                )
                .unwrap(),
            serde_json::json!([])
        );
        assert_eq!(
            fixture
                .invoke("owner_favorite_file_available", available.clone())
                .unwrap(),
            true
        );
        // The registered command's raw response is decoded as an array by invoke.
        assert_eq!(
            fixture
                .invoke("owner_read_favorite_file", available.clone())
                .unwrap(),
            serde_json::json!(bytes.as_slice())
        );
        assert_eq!(
            fixture
                .invoke("owner_save_favorite_file", export.clone())
                .unwrap(),
            serde_json::json!(export_path)
        );
        assert_eq!(std::fs::read(&export_path).unwrap(), bytes);

        let directory = fixture.root.path().join("downloads");
        std::fs::create_dir(&directory).unwrap();
        let existing = directory.join("attachment.bin");
        std::fs::write(&existing, b"keep existing download").unwrap();
        let saved = fixture.invoke("owner_save_favorite_file", serde_json::json!({"owner":"alice","fileConv":file_conv,"dest":directory,"toDirectory":true})).unwrap();
        let saved = std::path::PathBuf::from(saved.as_str().unwrap());
        assert_eq!(saved.parent(), Some(directory.as_path()));
        assert_ne!(saved, existing);
        assert_eq!(std::fs::read(&saved).unwrap(), bytes);
        assert_eq!(std::fs::read(&existing).unwrap(), b"keep existing download");

        // Queue commands with real retained plaintext behind the runtime gate.
        // A replacement owner or app lock must prevent both disclosure and writes.
        for change in ["switch-owner", "lock"] {
            for command in [
                "owner_favorite_file_available",
                "owner_read_favorite_file",
                "owner_save_favorite_file",
            ] {
                fixture.app_state.app_lock().clear();
                fixture
                    .app_state
                    .session()
                    .set("entry".into(), user("alice"), "pw".into());
                let denied_path = fixture
                    .root
                    .path()
                    .join(format!("denied-{change}-{command}"));
                let body = serde_json::json!({"owner":"alice","fileConv":file_conv,"dest":denied_path,"toDirectory":false});
                let (entered, captured) = std::sync::mpsc::channel();
                *fixture.app_state.owner_command_captured.lock().unwrap() = Some(entered);
                let lifecycle = fixture.node.0.blocking_write();
                let webview = fixture.webview.clone();
                let pending = std::thread::spawn(move || invoke(&webview, command, body));
                let captured = captured.recv_timeout(std::time::Duration::from_secs(2));
                if change == "switch-owner" {
                    fixture
                        .app_state
                        .session()
                        .set("replacement".into(), user("bob"), "pw".into());
                } else {
                    crate::app_lock::lock_app(
                        fixture.webview.state::<AppState>(),
                        fixture.webview.state::<crate::app_lock::AppLock>(),
                    )
                    .unwrap();
                }
                drop(lifecycle);
                let result = pending.join().unwrap();
                *fixture.app_state.owner_command_captured.lock().unwrap() = None;
                assert!(captured.is_ok(), "{command} captures entry before waiting");
                assert_eq!(
                    result.unwrap_err()["kind"],
                    "authorization",
                    "{change}: {command}"
                );
                assert!(
                    !denied_path.exists(),
                    "{change}: {command} must not export bytes"
                );
            }
        }
        fixture.app_state.app_lock().clear();
        fixture
            .app_state
            .session()
            .set("restored".into(), user("alice"), "pw".into());
        assert_eq!(
            fixture
                .invoke("owner_read_favorite_file", available.clone())
                .unwrap(),
            serde_json::json!(bytes.as_slice())
        );

        fixture.invoke("owner_remove_message_favorite", serde_json::json!({"owner":"alice","id":favorite["id"],"expectedRevision":favorite["revision"],"expectedDeviceId":favorite["device_id"]})).unwrap();
        assert_eq!(
            fixture
                .invoke("owner_favorite_file_available", available.clone())
                .unwrap(),
            false
        );
        assert!(
            fixture
                .invoke("owner_read_favorite_file", available)
                .is_err()
        );
        // Denial must leave an existing explicit destination untouched too.
        std::fs::write(&export_path, b"keep destination").unwrap();
        assert!(fixture.invoke("owner_save_favorite_file", export).is_err());
        assert_eq!(std::fs::read(&export_path).unwrap(), b"keep destination");
        let denied_path = fixture.root.path().join("removed-export.bin");
        assert!(fixture.invoke("owner_save_favorite_file", serde_json::json!({"owner":"alice","fileConv":file_conv,"dest":denied_path,"toDirectory":false})).is_err());
        assert!(!denied_path.exists());
    }

    #[test]
    fn registered_search_validates_inputs_and_explicit_owner() {
        let fixture = Fixture::new();
        for kind in ["account", "channel", "legacy-device"] {
            let length = if kind == "channel" { 32 } else { 16 };
            for (target, message_id) in [
                ("gg".repeat(length), serde_json::json!("01".repeat(32))),
                ("01".repeat(length), serde_json::json!("aa")),
                ("a".into(), serde_json::Value::Null),
            ] {
                assert_eq!(fixture.invoke("owner_search_context", serde_json::json!({"owner":"alice","reference":{"kind":kind,"target":target,"message_id":message_id}})).unwrap_err()["kind"], "invalid-input");
            }
            assert_eq!(fixture.invoke("owner_search_context", serde_json::json!({"owner":"bob","reference":{"kind":kind,"target":"01".repeat(length),"message_id":null}})).unwrap_err()["kind"], "authorization");
        }
        assert_eq!(
            fixture
                .invoke(
                    "owner_search_recent",
                    serde_json::json!({"owner":"bob","query":"token"})
                )
                .unwrap_err()["kind"],
            "authorization"
        );
        assert_eq!(
            fixture
                .invoke(
                    "owner_search_recent",
                    serde_json::json!({"owner":"alice","query":"é".repeat(2049)})
                )
                .unwrap_err()["kind"],
            "invalid-input"
        );
        assert!(
            fixture
                .invoke(
                    "owner_search_recent",
                    serde_json::json!({"owner":"alice","query":"é".repeat(2048)})
                )
                .is_ok()
        );
        for reference in [
            serde_json::json!({"kind":"other","target":"01".repeat(16),"message_id":null}),
            serde_json::json!({"kind":"account","message_id":null}),
            serde_json::json!({"kind":"account","target":"01".repeat(16)}),
            serde_json::json!({"kind":"account","target":42,"message_id":null}),
            serde_json::json!({"kind":"account","target":"01".repeat(16),"message_id":42}),
            serde_json::json!(null),
        ] {
            assert_eq!(
                fixture
                    .invoke(
                        "owner_search_context",
                        serde_json::json!({"owner":"alice","reference":reference})
                    )
                    .unwrap_err()["kind"],
                "invalid-input"
            );
        }
    }

    #[test]
    fn registered_search_caps_hits_and_context_serialized_bytes() {
        let fixture = Fixture::new();
        let channel = fixture
            .invoke(
                "create_channel",
                serde_json::json!({"name":"Caps", "memberIds":[]}),
            )
            .unwrap();
        for _ in 0..201 {
            fixture
                .invoke(
                    "send_channel_message",
                    serde_json::json!({"channelId":channel,"text":"cap token"}),
                )
                .unwrap();
        }
        let results = fixture
            .invoke(
                "owner_search_recent",
                serde_json::json!({"owner":"alice","query":"cap token"}),
            )
            .unwrap();
        assert_eq!(results["hits"].as_array().unwrap().len(), 200);
        assert_eq!(results["coverage"]["truncated"], true);
        let text = "oversize token ".to_owned() + &"x".repeat(60_000);
        for _ in 0..20 {
            fixture
                .invoke(
                    "send_channel_message",
                    serde_json::json!({"channelId":channel,"text":text}),
                )
                .unwrap();
        }
        let results = fixture
            .invoke(
                "owner_search_recent",
                serde_json::json!({"owner":"alice","query":"oversize token"}),
            )
            .unwrap();
        let reference = results["hits"][0]["reference"].clone();
        let response = fixture
            .invoke(
                "owner_search_context",
                serde_json::json!({"owner":"alice","reference":reference}),
            )
            .unwrap();
        assert_eq!(
            response,
            serde_json::json!({"status":"unavailable","reference":reference,"messages":[],"newer_entries":null})
        );
    }

    #[test]
    fn registered_search_recent_response_byte_cap_keeps_newest_prefix() {
        let fixture = Fixture::new();
        let channel = fixture
            .invoke(
                "create_channel",
                serde_json::json!({"name":"Bytes", "memberIds":[]}),
            )
            .unwrap();
        let text = "byte cap token ".to_owned() + &"x".repeat(60_000);
        for _ in 0..20 {
            fixture
                .invoke(
                    "send_channel_message",
                    serde_json::json!({"channelId":channel,"text":text}),
                )
                .unwrap();
        }
        let results = fixture
            .invoke(
                "owner_search_recent",
                serde_json::json!({"owner":"alice","query":"byte cap token"}),
            )
            .unwrap();
        assert!(serde_json::to_vec(&results).unwrap().len() <= 1024 * 1024);
        assert_eq!(results["coverage"]["truncated"], true);
        let hits = results["hits"].as_array().unwrap();
        assert!(!hits.is_empty() && hits.len() < 20);
        let history = fixture
            .invoke(
                "channel_history",
                serde_json::json!({"channelId":channel,"limit":25}),
            )
            .unwrap();
        assert_eq!(
            hits.iter()
                .map(|hit| hit["reference"]["message_id"].clone())
                .collect::<Vec<_>>(),
            history
                .as_array()
                .unwrap()
                .iter()
                .rev()
                .take(hits.len())
                .map(|entry| entry["id"].clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn registered_protected_click_reveal_redacts_history_and_rejects_plaintext() {
        let fixture = Fixture::new();
        let account = fixture.offline_peer();
        let id = fixture.invoke("owner_enqueue_text", serde_json::json!({"owner":"alice","account":account,"text":"protected secret","protection":"click"})).unwrap();
        let reference = serde_json::json!({"kind":"account","target":account,"message_id":id});
        let history = fixture
            .invoke(
                "owner_account_history",
                serde_json::json!({"owner":"alice","account":account,"limit":50}),
            )
            .unwrap();
        assert!(!history.to_string().contains("protected secret"));
        assert_eq!(history[0]["protection"], "click");
        let revealed = fixture
            .invoke(
                "owner_reveal_message",
                serde_json::json!({"owner":"alice","reference":reference}),
            )
            .unwrap();
        assert_eq!(revealed["text"], "protected secret");
        assert_eq!(revealed["visible_for_ms"], 30_000);
        fixture
            .invoke(
                "recall_message",
                serde_json::json!({"convId":account,"target":id,"isChannel":false}),
            )
            .unwrap();
        assert!(
            fixture
                .invoke(
                    "owner_reveal_message",
                    serde_json::json!({"owner":"alice","reference":reference})
                )
                .is_err()
        );
        let recalled = fixture
            .invoke(
                "owner_account_history",
                serde_json::json!({"owner":"alice","account":account,"limit":50}),
            )
            .unwrap();
        assert!(!recalled.to_string().contains("protected secret"));
        let plain = fixture
            .invoke(
                "owner_enqueue_text",
                serde_json::json!({"owner":"alice","account":account,"text":"ordinary"}),
            )
            .unwrap();
        assert!(fixture.invoke("owner_reveal_message", serde_json::json!({"owner":"alice","reference":{"kind":"account","target":account,"message_id":plain}})).is_err());
    }

    #[test]
    fn protected_image_authentication_failure_and_lock_transition_never_return_bytes() {
        use tauri::Manager;
        for change in ["cancel", "lock-clear"] {
            let fixture = Fixture::new();
            let account = fixture.offline_peer();
            let path = fixture.root.path().join("authenticated.png");
            std::fs::write(&path, b"\x89PNG\r\n\x1a\nsecret").unwrap();
            let accepted = fixture.invoke("owner_enqueue_file", serde_json::json!({"owner":"alice","account":account,"path":path,"media":true,"protection":"authenticate"})).unwrap();
            let reference = mesh_talk_core::node::SearchReference::Account {
                account_id: account,
                message_id: Some(super::parse_id(accepted["id"].as_str().unwrap()).unwrap()),
            };
            let admission = crate::runtime_admission::RuntimeAdmission::capture(
                &fixture.app_state,
                &fixture.node,
                "alice",
            )
            .unwrap();
            let lock = fixture.app_state.app_lock().clone();
            let revision = lock.authorization_revision().unwrap();
            let result = tauri::async_runtime::block_on(super::search::reveal_image_with_verifier(
                admission,
                lock.clone(),
                revision,
                reference,
                || async {
                    if change == "cancel" {
                        return Err(crate::commands::CommandError::Authentication(
                            "canceled".into(),
                        ));
                    }
                    crate::app_lock::lock_app(
                        fixture.webview.state::<AppState>(),
                        fixture.webview.state::<crate::app_lock::AppLock>(),
                    )
                    .unwrap();
                    lock.clear();
                    Ok(())
                },
            ));
            assert!(result.is_err(), "{change} never returns image bytes");
        }
    }

    #[test]
    fn registered_protected_paste_retry_preserves_source_until_durable_acceptance() {
        let fixture = Fixture::new();
        let account = fixture.offline_peer();
        let channel = fixture
            .invoke(
                "create_channel",
                serde_json::json!({"name":"Retry images","memberIds":[]}),
            )
            .unwrap();
        for protection in [serde_json::Value::Null, serde_json::json!("click")] {
            for (command, key, valid, invalid) in [
                (
                    "owner_enqueue_file",
                    "account",
                    serde_json::json!(account),
                    serde_json::json!("ab".repeat(16)),
                ),
                (
                    "owner_enqueue_channel_file",
                    "channelId",
                    channel.clone(),
                    serde_json::json!("ab".repeat(32)),
                ),
            ] {
                let source = fixture.invoke("write_temp_file", serde_json::json!({"bytes":b"\x89PNG\r\n\x1a\nretry image".as_slice(),"ext":"png","name":"retry.png"})).unwrap();
                let path = std::path::PathBuf::from(source.as_str().unwrap());
                let mut body = serde_json::json!({"owner":"alice","path":source,"media":true,"protection":protection});
                body[key] = invalid;
                let mut retired = body.clone();
                retired["owner"] = serde_json::json!("retired");
                assert!(fixture.invoke(command, retired).is_err());
                assert!(
                    path.is_file(),
                    "{command} preserves source after owner rejection"
                );
                let mut malformed = body.clone();
                malformed[key] = serde_json::json!("malformed");
                assert!(fixture.invoke(command, malformed).is_err());
                assert!(
                    path.is_file(),
                    "{command} preserves source after malformed destination"
                );
                assert!(
                    fixture.invoke(command, body.clone()).is_err(),
                    "{command} rejects missing destination"
                );
                assert!(
                    path.is_file(),
                    "{command} preserves retry source after rejection"
                );
                body[key] = valid;
                let accepted = fixture.invoke(command, body).unwrap();
                assert_eq!(accepted["id"].as_str().unwrap().len(), 64);
                assert!(
                    !path.exists(),
                    "{command} removes staging source only after durable acceptance"
                );
            }
        }
    }

    #[test]
    fn registered_protected_image_reveal_requires_exact_authority_and_blocks_ordinary_reads() {
        let fixture = Fixture::new();
        let account = fixture.offline_peer();
        let channel = fixture
            .invoke(
                "create_channel",
                serde_json::json!({"name":"Protected images","memberIds":[]}),
            )
            .unwrap();
        let path = fixture.root.path().join("protected.png");
        let bytes = b"\x89PNG\r\n\x1a\nprotected image bytes";
        std::fs::write(&path, bytes).unwrap();
        for (kind, target, command, key) in [
            (
                "account",
                serde_json::json!(account),
                "owner_enqueue_file",
                "account",
            ),
            (
                "channel",
                channel,
                "owner_enqueue_channel_file",
                "channelId",
            ),
        ] {
            let mut body =
                serde_json::json!({"owner":"alice","path":path,"media":true,"protection":"click"});
            body[key] = target.clone();
            let accepted = fixture.invoke(command, body).unwrap();
            let reference =
                serde_json::json!({"kind":kind,"target":target,"message_id":accepted["id"]});
            let revealed = fixture
                .invoke(
                    "owner_reveal_image",
                    serde_json::json!({"owner":"alice","reference":reference}),
                )
                .unwrap();
            assert_eq!(revealed, serde_json::to_value(bytes.as_slice()).unwrap());
            assert!(
                fixture
                    .invoke(
                        "owner_reveal_message",
                        serde_json::json!({"owner":"alice","reference":reference})
                    )
                    .is_err()
            );
            for read in ["read_media", "read_file"] {
                assert!(
                    fixture
                        .invoke(read, serde_json::json!({"fileConv":accepted["fileConv"]}))
                        .is_err(),
                    "{kind}: {read}"
                );
            }
            assert_eq!(
                fixture
                    .invoke(
                        "owner_reveal_image",
                        serde_json::json!({"owner":"bob","reference":reference})
                    )
                    .unwrap_err()["kind"],
                "authorization"
            );
        }
        let ordinary_content = b"\x89PNG\r\n\x1a\nordinary image bytes";
        std::fs::write(&path, ordinary_content).unwrap();
        let ordinary = fixture
            .invoke(
                "owner_enqueue_file",
                serde_json::json!({"owner":"alice","account":account,"path":path,"media":true}),
            )
            .unwrap();
        assert!(fixture.invoke("owner_reveal_image", serde_json::json!({"owner":"alice","reference":{"kind":"account","target":account,"message_id":ordinary["id"]}})).is_err());
        let ordinary_bytes = fixture
            .invoke(
                "read_media",
                serde_json::json!({"fileConv":ordinary["fileConv"]}),
            )
            .unwrap();
        assert_eq!(
            ordinary_bytes,
            serde_json::to_value(ordinary_content.as_slice()).unwrap()
        );
    }

    #[test]
    fn registered_protected_channel_reveal_preserves_namespace_and_redacts_history() {
        let fixture = Fixture::new();
        let channel = fixture
            .invoke(
                "create_channel",
                serde_json::json!({"name":"Protected", "memberIds":[]}),
            )
            .unwrap();
        let id = fixture.invoke("owner_enqueue_channel_text", serde_json::json!({"owner":"alice","channelId":channel,"text":"channel protected secret","protection":"click"})).unwrap();
        let history = fixture
            .invoke(
                "channel_history",
                serde_json::json!({"channelId":channel,"limit":10}),
            )
            .unwrap();
        assert!(!history.to_string().contains("channel protected secret"));
        assert_eq!(history[0]["protection"], "click");
        let revealed = fixture.invoke("owner_reveal_message", serde_json::json!({"owner":"alice","reference":{"kind":"channel","target":channel,"message_id":id}})).unwrap();
        assert_eq!(revealed["text"], "channel protected secret");
    }

    #[test]
    fn protected_authentication_rechecks_authority_runtime_and_retention() {
        use mesh_talk_core::node::SearchReference;
        use tauri::Manager;
        for change in [
            "success",
            "cancel",
            "lock",
            "lock-clear",
            "relogin",
            "recall",
            "replace-runtime",
            "expire",
        ] {
            let fixture = Fixture::new();
            let account = fixture.offline_peer();
            let expires = (change == "expire").then(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64
                    + 1500
            });
            let id = fixture.invoke("owner_enqueue_text", serde_json::json!({"owner":"alice","account":account,"text":"authenticated secret","protection":"authenticate","expiresAtMs":expires})).unwrap();
            let reference = SearchReference::Account {
                account_id: account.clone(),
                message_id: Some(super::parse_id(id.as_str().unwrap()).unwrap()),
            };
            let admission = crate::runtime_admission::RuntimeAdmission::capture(
                &fixture.app_state,
                &fixture.node,
                "alice",
            )
            .unwrap();
            let lock = fixture.app_state.app_lock().clone();
            let revision = lock.authorization_revision().unwrap();
            let replacement = (change == "replace-runtime").then(Fixture::new);
            let response =
                tauri::async_runtime::block_on(super::search::reveal_message_with_verifier(
                    admission,
                    lock.clone(),
                    revision,
                    reference,
                    || async {
                        match change {
                            "cancel" => {
                                return Err(crate::commands::CommandError::Authentication(
                                    "canceled".into(),
                                ));
                            }
                            "lock" | "lock-clear" => {
                                crate::app_lock::lock_app(
                                    fixture.webview.state::<AppState>(),
                                    fixture.webview.state::<crate::app_lock::AppLock>(),
                                )
                                .unwrap();
                                if change == "lock-clear" {
                                    lock.clear();
                                }
                            }
                            "relogin" => fixture.app_state.session().set(
                                "replacement".into(),
                                user("alice"),
                                "pw".into(),
                            ),
                            "recall" => {
                                let handle = fixture.node.0.read().await.as_ref().unwrap().handle();
                                handle
                                    .recall_account_if(
                                        &account,
                                        super::parse_id(id.as_str().unwrap()).unwrap(),
                                        |commit| commit(),
                                    )
                                    .await
                                    .unwrap();
                            }
                            "replace-runtime" => {
                                let new = replacement.as_ref().unwrap().node.0.write().await.take();
                                let old =
                                    std::mem::replace(&mut *fixture.node.0.write().await, new);
                                if let Some(old) = old {
                                    old.stop().await;
                                }
                            }
                            "expire" => {
                                tokio::time::sleep(std::time::Duration::from_millis(1600)).await
                            }
                            _ => {}
                        }
                        Ok(())
                    },
                ));
            if change == "success" {
                assert_eq!(
                    serde_json::to_value(response.unwrap()).unwrap()["text"],
                    "authenticated secret"
                );
                assert!(!lock.is_locked(), "revealing does not alter app lock");
            } else {
                assert!(response.is_err(), "{change} must never return plaintext");
            }
        }
    }

    #[test]
    fn registered_protected_reveal_requires_exact_reference_and_current_owner() {
        let fixture = Fixture::new();
        for command in ["owner_reveal_message", "owner_reveal_image"] {
            for reference in [
                serde_json::json!({"kind":"account","target":"01".repeat(16),"message_id":null}),
                serde_json::json!({"kind":"legacy-device","target":"01".repeat(16),"message_id":"01".repeat(32)}),
                serde_json::json!({"kind":"channel","target":"bad","message_id":"01".repeat(32)}),
            ] {
                assert_eq!(
                    fixture
                        .invoke(
                            command,
                            serde_json::json!({"owner":"alice","reference":reference})
                        )
                        .unwrap_err()["kind"],
                    "invalid-input"
                );
            }
            assert_eq!(fixture.invoke(command, serde_json::json!({"owner":"bob","reference":{"kind":"account","target":"01".repeat(16),"message_id":"01".repeat(32)}})).unwrap_err()["kind"], "authorization");
        }
    }

    #[test]
    fn registered_search_rejects_queued_replaced_owner_and_app_lock() {
        use tauri::Manager;
        let fixture = Fixture::new();
        for change in ["relogin", "logout", "lock"] {
            for (command, body) in [
                (
                    "owner_search_recent",
                    serde_json::json!({"owner":"alice","query":"secret"}),
                ),
                (
                    "owner_reveal_message",
                    serde_json::json!({"owner":"alice","reference":{"kind":"account","target":"01".repeat(16),"message_id":"01".repeat(32)}}),
                ),
                (
                    "owner_reveal_image",
                    serde_json::json!({"owner":"alice","reference":{"kind":"account","target":"01".repeat(16),"message_id":"01".repeat(32)}}),
                ),
                (
                    "owner_search_context",
                    serde_json::json!({"owner":"alice","reference":{"kind":"account","target":"01".repeat(16),"message_id":"01".repeat(32)}}),
                ),
            ] {
                fixture.app_state.app_lock().clear();
                fixture
                    .app_state
                    .session()
                    .set("entry".into(), user("alice"), "pw".into());
                let (entered, captured) = std::sync::mpsc::channel();
                *fixture.app_state.owner_command_captured.lock().unwrap() = Some(entered);
                let lifecycle = fixture.node.0.blocking_write();
                let webview = fixture.webview.clone();
                let pending = std::thread::spawn(move || invoke(&webview, command, body));
                let captured = captured.recv_timeout(std::time::Duration::from_secs(2));
                match change {
                    "logout" => fixture.app_state.session().clear(),
                    "relogin" => fixture.app_state.session().set(
                        "replacement".into(),
                        user("alice"),
                        "pw".into(),
                    ),
                    _ => crate::app_lock::lock_app(
                        fixture.webview.state::<AppState>(),
                        fixture.webview.state::<crate::app_lock::AppLock>(),
                    )
                    .unwrap(),
                }
                drop(lifecycle);
                let result = pending.join().unwrap();
                assert!(captured.is_ok(), "{command} captures before waiting");
                assert_eq!(
                    result.unwrap_err()["kind"],
                    "authorization",
                    "{command}: {change}"
                );
                *fixture.app_state.owner_command_captured.lock().unwrap() = None;
            }
        }
    }

    #[test]
    fn registered_own_device_and_legacy_device_history_remain_untracked() {
        use mesh_talk_core::{
            discovery::Announce,
            identity::{account::Account, device::DeviceIdentity},
        };
        let fixture = Fixture::new();
        let own = mesh_talk_core::identity::account_keystore::load(
            &fixture.root.path().join("accounts/alice/account.keystore"),
            "pw",
        )
        .unwrap();
        fixture.announce_peer(Announce::new_with_account(
            &DeviceIdentity::generate(),
            &own,
            "Own other device",
            1,
        ));
        let own_id = fixture.invoke("owner_enqueue_text", serde_json::json!({"owner":"alice", "account":own.account_id(), "text":"own sync", "replyTo":null})).unwrap();
        let status = fixture
            .invoke(
                "owner_delivery_statuses",
                serde_json::json!({"owner":"alice", "account":own.account_id(), "ids":[own_id]}),
            )
            .unwrap();
        assert_eq!(
            status,
            serde_json::json!([]),
            "own-device messages do not have an external delivery claim"
        );
        let legacy_device = DeviceIdentity::generate();
        let legacy_account = Account::generate();
        fixture.announce_peer(Announce::new_with_account(
            &legacy_device,
            &legacy_account,
            "Legacy device path",
            1,
        ));
        let legacy_id = tauri::async_runtime::block_on(async {
            let guard = fixture.node.0.write().await;
            let rt = guard.as_ref().unwrap();
            rt.handle()
                .send_dm(
                    &legacy_device.public().user_id(),
                    b"legacy device addressed",
                )
                .await
                .unwrap();
            rt.history(&legacy_device.public(), 10)[0].id
        });
        let status = fixture.invoke("owner_delivery_statuses", serde_json::json!({"owner":"alice", "account":legacy_account.account_id(), "ids":[hex::encode(legacy_id.as_bytes())]})).unwrap();
        assert_eq!(status, serde_json::json!([]));
    }

    #[test]
    fn registered_status_query_does_not_track_real_incoming_account_message() {
        let fixture = Fixture::new();
        let remote_root = tempfile::tempdir().unwrap();
        let reservation = reserve_discovery_port();
        let port = reservation.local_addr().unwrap().port();
        let remote = tauri::async_runtime::block_on(mesh_talk_core::node::NodeRuntime::start(
            remote_root.path(),
            "sender",
            "Sender",
            "pw",
            port,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        ))
        .unwrap();
        drop(reservation);
        fixture.announce_peer(
            remote
                .handle()
                .signed_announce("Sender", remote.listen_tcp_port()),
        );
        let (announce, account, device) = {
            let guard = fixture.node.0.blocking_write();
            let rt = guard.as_ref().unwrap();
            (
                rt.handle().signed_announce("Alice", rt.listen_tcp_port()),
                rt.account_id().to_owned(),
                rt.user_id().to_owned(),
            )
        };
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .send_to(
                &mesh_talk_core::discovery::announce::encode(&announce),
                (std::net::Ipv4Addr::LOCALHOST, port),
            )
            .unwrap();
        tauri::async_runtime::block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while remote.peer_public(&device).is_none() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            remote
                .handle()
                .send_to_account(&account, b"real incoming account message", None)
                .await
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while fixture
                    .node
                    .0
                    .write()
                    .await
                    .as_ref()
                    .unwrap()
                    .account_history(remote.account_id(), 10)
                    .is_empty()
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        });
        let history = fixture
            .invoke(
                "owner_account_history",
                serde_json::json!({"owner":"alice", "account":remote.account_id(), "limit":10}),
            )
            .unwrap();
        assert_eq!(history[0]["from_me"], false);
        let statuses = fixture.invoke("owner_delivery_statuses", serde_json::json!({"owner":"alice", "account":remote.account_id(), "ids":[history[0]["id"]]})).unwrap();
        assert_eq!(statuses, serde_json::json!([]));
        tauri::async_runtime::block_on(remote.stop());
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let runtime = self.node.0.blocking_write().take();
            if let Some(runtime) = runtime {
                tauri::async_runtime::block_on(runtime.stop());
            }
        }
    }

    fn invoke(
        webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
        cmd: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, serde_json::Value> {
        tauri::test::get_ipc_response(
            webview,
            tauri::webview::InvokeRequest {
                cmd: cmd.into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: if cfg!(windows) {
                    "http://tauri.localhost"
                } else {
                    "tauri://localhost"
                }
                .parse()
                .unwrap(),
                body: tauri::ipc::InvokeBody::Json(body),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.into(),
            },
        )
        .map(|body| body.deserialize::<serde_json::Value>().unwrap())
    }

    #[test]
    fn registered_channel_enqueues_return_exact_ids_and_reject_retired_owners() {
        let fixture = Fixture::new();
        let channel = fixture
            .invoke(
                "create_channel",
                serde_json::json!({"name":"Acceptance", "memberIds":[]}),
            )
            .unwrap();
        let path = fixture.root.path().join("photo.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nphoto").unwrap();
        let cases = [
            (
                "owner_enqueue_channel_text",
                serde_json::json!({"owner":"alice","channelId":channel,"text":"kk","replyTo":null}),
            ),
            (
                "owner_enqueue_channel_sticker",
                serde_json::json!({"owner":"alice","channelId":channel,"stickerId":"heart","fallback":"heart"}),
            ),
            (
                "owner_enqueue_channel_file",
                serde_json::json!({"owner":"alice","channelId":channel,"path":path,"media":true}),
            ),
        ];
        let mut ids = Vec::new();
        for (command, body) in cases {
            let accepted = fixture.invoke(command, body.clone()).unwrap();
            let id = if command.ends_with("file") {
                accepted["id"].clone()
            } else {
                accepted
            };
            assert_eq!(id.as_str().unwrap().len(), 64);
            ids.push(id);
            let mut retired = body;
            retired["owner"] = serde_json::json!("retired");
            assert!(fixture.invoke(command, retired).is_err());
        }
        let history = fixture
            .invoke(
                "channel_history",
                serde_json::json!({"channelId":channel,"limit":10}),
            )
            .unwrap();
        for id in ids {
            assert!(
                history
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row["id"] == id)
            );
        }
    }

    #[test]
    fn registered_enqueues_return_stable_ids_and_exact_scope_awaiting_statuses() {
        let fixture = Fixture::new();
        let account = fixture.offline_peer();
        let text = fixture.invoke("owner_enqueue_text", serde_json::json!({"owner":"alice", "account":account, "text":"durable offline", "replyTo":null})).unwrap();
        let sticker = fixture.invoke("owner_enqueue_sticker", serde_json::json!({"owner":"alice", "account":account, "stickerId":"wave", "fallback":"hello"})).unwrap();
        let path = fixture.root.path().join("card.txt");
        std::fs::write(&path, b"card bytes").unwrap();
        let file = fixture
            .invoke(
                "owner_enqueue_file",
                serde_json::json!({"owner":"alice", "account":account, "path":path, "media":false}),
            )
            .unwrap();
        let ids = vec![text, sticker, file["id"].clone()];
        assert!(ids.iter().all(|id| id.as_str().unwrap().len() == 64));
        assert_eq!(file["fileConv"].as_str().unwrap().len(), 64);
        let statuses = fixture
            .invoke(
                "owner_delivery_statuses",
                serde_json::json!({"owner":"alice", "account":account, "ids":ids}),
            )
            .unwrap();
        assert_eq!(statuses.as_array().unwrap().len(), 3);
        assert!(
            statuses
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["status"] == "awaiting")
        );
        // This fixture owns a live runtime. Local scheduling can run before
        // this snapshot even though the remote recipient cannot issue a receipt.
        for item in statuses.as_array().unwrap() {
            assert!(ids.contains(&item["id"]));
            if let Some(observation) = item.get("observation") {
                assert!(
                    observation
                        .as_object()
                        .unwrap()
                        .keys()
                        .all(|key| matches!(key.as_str(), "kind" | "age_ms" | "reason"))
                );
                if let Some(reason) = observation.get("reason") {
                    assert!(matches!(
                        reason.as_str(),
                        Some("post_office_disabled" | "no_known_route" | "attempt_timed_out")
                    ));
                }
                assert!(matches!(
                    observation["kind"].as_str(),
                    Some("attempting" | "retry_scheduled")
                ));
                assert!(observation["age_ms"].as_u64().is_some());
            }
        }
        let file_status = statuses
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == file["id"])
            .unwrap();
        assert_eq!(
            file_status["file_progress"],
            serde_json::json!({
                "done": 0,
                "total": 1,
                "delivered": false
            })
        );
        let history = fixture
            .invoke(
                "owner_account_history",
                serde_json::json!({"owner":"alice", "account":account, "limit":500}),
            )
            .unwrap();
        let history_ids: Vec<_> = history
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].clone())
            .collect();
        assert_eq!(history_ids.len(), 3);
        assert!(ids.iter().all(|id| history_ids.contains(id)));
        let elsewhere = fixture
            .invoke(
                "owner_delivery_statuses",
                serde_json::json!({"owner":"alice", "account":"11".repeat(16), "ids":ids}),
            )
            .unwrap();
        assert_eq!(elsewhere, serde_json::json!([]));
        let malformed = fixture
            .invoke(
                "owner_delivery_statuses",
                serde_json::json!({"owner":"alice", "account":account, "ids":["not-hex"]}),
            )
            .unwrap_err();
        assert_eq!(malformed["kind"], "invalid-input");
        let too_many = fixture.invoke("owner_delivery_statuses", serde_json::json!({"owner":"alice", "account":account, "ids":vec!["11".repeat(32);257]})).unwrap_err();
        assert_eq!(too_many["kind"], "invalid-input");
        let input = fixture.invoke("owner_enqueue_text", serde_json::json!({"owner":"alice", "account":account, "text":"x".repeat(128*1024), "replyTo":null})).unwrap_err();
        assert_eq!(input["kind"], "invalid-input");
        assert!(fixture.invoke("owner_enqueue_text", serde_json::json!({"owner":"bob", "account":account, "text":"reject", "replyTo":null})).is_err());
        fixture.app_state.session().clear();
        assert!(
            fixture
                .invoke(
                    "owner_delivery_statuses",
                    serde_json::json!({"owner":"alice", "account":account, "ids":ids})
                )
                .is_err()
        );
    }

    #[test]
    fn registered_sync_mutations_reject_a_different_session_before_persisting() {
        let fixture = Fixture::new();
        let before = fixture.sync_documents();
        fixture
            .app_state
            .session()
            .set("b".into(), user("bob"), "pw".into());
        for (command, body) in [
            (
                "set_device_sync_category",
                serde_json::json!({"owner":"alice","category":"history", "enabled":false}),
            ),
            (
                "set_account_preference",
                serde_json::json!({"owner":"alice","category":"appearance", "key":"theme", "value":"dark"}),
            ),
        ] {
            let result = fixture.invoke(command, body);
            assert!(
                result.is_err(),
                "{command} accepted a mismatched runtime owner: {result:?}"
            );
            assert_eq!(result.unwrap_err()["kind"], "authorization");
        }
        let guard = fixture.node.0.blocking_write();
        let node = guard.as_ref().unwrap().handle();
        assert!(node.device_sync_policy().history);
        assert!(
            node.account_preferences(mesh_talk_core::node::SyncCategory::Appearance)
                .is_empty()
        );
        drop(guard);
        assert_eq!(fixture.sync_documents(), before);
    }

    #[test]
    fn legacy_account_sends_preserve_null_results_and_durable_history() {
        let fixture = Fixture::new();
        let account = fixture.offline_peer();
        for (command, body) in [
            (
                "send_to_account",
                serde_json::json!({"account":account,"text":"legacy text","replyTo":null}),
            ),
            (
                "send_sticker",
                serde_json::json!({"convId":account,"stickerId":"wave","fallback":"hello","isChannel":false}),
            ),
        ] {
            assert_eq!(
                fixture.invoke(command, body).unwrap(),
                serde_json::Value::Null
            );
        }
        let path = fixture.root.path().join("legacy-file.txt");
        std::fs::write(&path, b"legacy file content").unwrap();
        let file_conv = fixture
            .invoke(
                "send_file_to_account",
                serde_json::json!({"account":account,"path":path,"media":false}),
            )
            .unwrap();
        let bytes = hex::decode(file_conv.as_str().unwrap()).unwrap();
        assert_eq!(bytes.len(), 32);
        assert!(
            path.exists(),
            "ordinary user source files must remain intact"
        );
        let runtime = fixture.node.0.blocking_write();
        assert_eq!(
            runtime
                .as_ref()
                .unwrap()
                .account_history(&account, 10)
                .len(),
            3
        );
    }

    #[test]
    fn legacy_account_sends_reject_a_runtime_from_another_session() {
        let fixture = Fixture::new();
        let account = fixture.offline_peer();
        let path = fixture.root.path().join("must-not-stage.txt");
        std::fs::write(&path, b"old runtime must not stage this file").unwrap();
        fixture
            .app_state
            .session()
            .set("bob-token".into(), user("bob"), "pw".into());
        for (command, body) in [
            (
                "send_file_to_account",
                serde_json::json!({"account":account,"path":path,"media":false}),
            ),
            (
                "send_to_account",
                serde_json::json!({"account":account,"text":"must not send","replyTo":null}),
            ),
            (
                "send_sticker",
                serde_json::json!({"convId":account,"stickerId":"wave","fallback":"hello","isChannel":false}),
            ),
        ] {
            let result = fixture.invoke(command, body);
            assert_eq!(result.unwrap_err()["kind"], "authorization", "{command}");
        }
        let runtime = fixture.node.0.blocking_write();
        assert!(
            runtime
                .as_ref()
                .unwrap()
                .account_history(&account, 10)
                .is_empty()
        );
    }

    #[test]
    fn canceled_blocking_waiter_keeps_runtime_owned_until_writer_finishes() {
        let fixture = Fixture::new();
        let admission = crate::runtime_admission::RuntimeAdmission::capture_current(
            &fixture.app_state,
            &fixture.node,
        )
        .unwrap();
        let (entered, waiting) = std::sync::mpsc::channel();
        let (release, proceed) = std::sync::mpsc::channel();
        let (finished, done) = std::sync::mpsc::channel();
        tauri::async_runtime::block_on(async {
            let pending = tokio::spawn(async move {
                admission
                    .blocking(move |_, _| {
                        entered.send(()).unwrap();
                        proceed.recv().unwrap();
                        finished.send(()).unwrap();
                        Ok(())
                    })
                    .await
            });
            waiting
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            pending.abort();
            assert!(pending.await.unwrap_err().is_cancelled());
            assert!(
                fixture.node.0.try_write().is_err(),
                "retirement must wait for the real writer after cancellation"
            );
            release.send(()).unwrap();
        });
        done.recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        // The writer releases session and runtime guards before retirement proceeds.
        drop(fixture.node.0.blocking_write());
    }

    #[test]
    fn registered_channel_history_policy_round_trips_and_rejects_locked_mutation() {
        use tauri::Manager;
        let fixture = Fixture::new();
        let channel = fixture
            .invoke(
                "create_channel",
                serde_json::json!({"name":"History settings", "memberIds":[]}),
            )
            .unwrap();
        for policy in [
            "off",
            "last_ten",
            "last_three_days",
            "last_ten_within_three_days",
        ] {
            fixture
                .invoke(
                    "set_channel_history_policy",
                    serde_json::json!({"channelId":channel,"policy":policy}),
                )
                .unwrap();
            let info = fixture
                .invoke("channel_members", serde_json::json!({"channelId":channel}))
                .unwrap();
            assert_eq!(info["history_policy"], policy);
        }
        assert!(
            fixture
                .invoke(
                    "set_channel_history_policy",
                    serde_json::json!({"channelId":channel,"policy":"unbounded"})
                )
                .is_err()
        );
        crate::app_lock::lock_app(
            fixture.webview.state::<AppState>(),
            fixture.webview.state::<crate::app_lock::AppLock>(),
        )
        .unwrap();
        assert_eq!(
            fixture
                .invoke(
                    "set_channel_history_policy",
                    serde_json::json!({"channelId":channel,"policy":"off"})
                )
                .unwrap_err()["kind"],
            "authorization"
        );
        let runtime = fixture.node.0.blocking_read();
        let channel_id = crate::chat_commands::parse_channel_id(channel.as_str().unwrap()).unwrap();
        assert_eq!(
            runtime
                .as_ref()
                .unwrap()
                .handle()
                .channel_history_policy(channel_id),
            mesh_talk_core::channel::HistoryPolicy::LastTenWithinThreeDays
        );
    }

    #[test]
    fn queued_channel_mutation_rejects_lock_and_same_owner_relogin() {
        use tauri::Manager;
        let fixture = Fixture::new();
        let channel = "11".repeat(32);
        let cases = [
            (
                "write_temp_file",
                serde_json::json!({"bytes":[],"ext":"png"}),
            ),
            ("rescan_peers", serde_json::json!({})),
            (
                "probe_peer_endpoint",
                serde_json::json!({"host":"127.0.0.1","port":47474}),
            ),
            (
                "create_channel",
                serde_json::json!({"name":"must not create", "memberIds":[]}),
            ),
            (
                "rename_channel",
                serde_json::json!({"channelId":channel, "name":"must not rename"}),
            ),
            (
                "set_channel_history_policy",
                serde_json::json!({"channelId":channel,"policy":"off"}),
            ),
            (
                "set_channel_replica_enabled",
                serde_json::json!({"channelId":channel,"enabled":false}),
            ),
            (
                "add_channel_member",
                serde_json::json!({"channelId":channel,"memberId":"peer"}),
            ),
            (
                "remove_channel_member",
                serde_json::json!({"channelId":channel,"memberId":"peer"}),
            ),
            (
                "remove_channel_account_members",
                serde_json::json!({"channelId":channel,"accountId":"peer-account"}),
            ),
            (
                "transfer_channel_owner",
                serde_json::json!({"channelId":channel,"memberId":"peer"}),
            ),
            ("leave_channel", serde_json::json!({"channelId":channel})),
            (
                "accept_channel_invite",
                serde_json::json!({"channelId":channel}),
            ),
            ("dissolve_channel", serde_json::json!({"channelId":channel})),
            (
                "send_channel_message",
                serde_json::json!({"channelId":channel,"text":"queued"}),
            ),
            (
                "react_channel",
                serde_json::json!({"channelId":channel,"target":channel,"emoji":"x","remove":false}),
            ),
            (
                "send_dm",
                serde_json::json!({"recipient":"peer","text":"queued"}),
            ),
            (
                "send_call_signal",
                serde_json::json!({"target":"peer","payload":"queued"}),
            ),
            (
                "react_dm",
                serde_json::json!({"recipient":"peer","target":channel,"emoji":"x","remove":false}),
            ),
            (
                "react_account",
                serde_json::json!({"account":"peer","target":channel,"emoji":"x","remove":false}),
            ),
            (
                "delete_message",
                serde_json::json!({"convId":channel,"target":channel,"isChannel":true}),
            ),
            (
                "recall_message",
                serde_json::json!({"convId":channel,"target":channel,"isChannel":true}),
            ),
            (
                "clear_conversation",
                serde_json::json!({"convId":channel,"isChannel":true}),
            ),
            ("start_linking", serde_json::json!({})),
            ("stop_linking", serde_json::json!({})),
            (
                "link_device",
                serde_json::json!({"peer":"peer","code":"unused"}),
            ),
            ("rekey_account", serde_json::json!({})),
            (
                "publish_avatar",
                serde_json::json!({"owner":"alice","expectedAccount":fixture.node.0.blocking_read().as_ref().unwrap().account_id(),"avatar":"queued"}),
            ),
            (
                "set_attachment_global_auto",
                serde_json::json!({"auto":false}),
            ),
            (
                "set_attachment_contact_auto",
                serde_json::json!({"account":"peer","auto":false}),
            ),
            (
                "decide_attachment",
                serde_json::json!({"fileConv":channel,"accept":true}),
            ),
            (
                "send_file_dm",
                serde_json::json!({"recipient":"peer","path":"unused","media":false}),
            ),
            (
                "send_file_channel",
                serde_json::json!({"channelId":channel,"path":"unused","media":false}),
            ),
            (
                "save_file",
                serde_json::json!({"fileConv":channel,"dest":"unused"}),
            ),
            (
                "save_file_to_dir",
                serde_json::json!({"fileConv":channel,"dir":"unused"}),
            ),
        ];
        for lock in [true, false] {
            for (command, body) in &cases {
                fixture.app_state.app_lock().clear();
                let (entered_tx, entered_rx) = std::sync::mpsc::channel();
                *fixture.app_state.owner_command_captured.lock().unwrap() = Some(entered_tx);
                let lifecycle = fixture.node.0.blocking_write();
                let webview = fixture.webview.clone();
                let command = *command;
                let body = body.clone();
                let pending = std::thread::spawn(move || invoke(&webview, command, body));
                let captured = entered_rx.recv_timeout(std::time::Duration::from_secs(2));
                if lock {
                    crate::app_lock::lock_app(
                        fixture.webview.state::<AppState>(),
                        fixture.webview.state::<crate::app_lock::AppLock>(),
                    )
                    .unwrap();
                } else {
                    fixture.app_state.session().set(
                        "replacement".into(),
                        user("alice"),
                        "pw".into(),
                    );
                }
                drop(lifecycle);
                let result = pending.join().unwrap();
                assert!(
                    captured.is_ok(),
                    "{command} must capture entry authority before waiting"
                );
                assert_eq!(
                    result.unwrap_err()["kind"],
                    "authorization",
                    "queued {command}, lock={lock}"
                );
                assert!(
                    fixture
                        .node
                        .0
                        .blocking_write()
                        .as_ref()
                        .unwrap()
                        .list_channels()
                        .is_empty()
                );
            }
        }
    }

    fn account_profile_command_cases(owner: &str) -> Vec<(&'static str, serde_json::Value)> {
        vec![
            (
                "get_account_profile",
                serde_json::json!({"owner":owner,"accountId":"00".repeat(16)}),
            ),
            (
                "save_account_bio",
                serde_json::json!({"owner":owner,"bio":"Hello"}),
            ),
        ]
    }

    #[test]
    fn registered_account_profile_commands_reject_wrong_owner_and_locked_app() {
        use tauri::Manager;
        let fixture = Fixture::new();
        for (command, body) in account_profile_command_cases("bob") {
            let response = fixture.invoke(command, body);
            assert!(
                matches!(response, Err(ref error) if error["kind"] == "authorization"),
                "profile commands must reject another owner before reading profiles"
            );
        }
        crate::app_lock::lock_app(
            fixture.webview.state::<AppState>(),
            fixture.webview.state::<crate::app_lock::AppLock>(),
        )
        .unwrap();
        for (command, body) in account_profile_command_cases("alice") {
            let response = fixture.invoke(command, body);
            assert!(
                matches!(response, Err(ref error) if error["kind"] == "authorization"),
                "profile commands must reject a locked app before reading or saving profiles"
            );
        }
    }

    #[test]
    fn account_profile_save_returns_after_local_commit_with_a_stalled_peer() {
        use mesh_talk_core::identity::{account::Account, device::DeviceIdentity};
        use std::sync::atomic::{AtomicBool, Ordering};
        let fixture = Fixture::new();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let release = std::sync::Arc::new(AtomicBool::new(false));
        let released = release.clone();
        let stalled = std::thread::spawn(move || {
            let mut sockets = Vec::new();
            while !released.load(Ordering::Acquire) {
                if let Ok((socket, _)) = listener.accept() {
                    sockets.push(socket);
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });
        let peer = DeviceIdentity::generate();
        let account = Account::generate();
        fixture.announce_peer(mesh_talk_core::discovery::Announce::new_with_account(
            &peer, &account, "Stalled", port,
        ));
        fixture.invoke("owner_enqueue_text",serde_json::json!({"owner":"alice","account":account.account_id(),"text":"grant contact","replyTo":null})).unwrap();
        let own = fixture
            .node
            .0
            .blocking_read()
            .as_ref()
            .unwrap()
            .account_id()
            .to_owned();
        let _card = fixture
            .invoke(
                "get_account_profile",
                serde_json::json!({"owner":"alice","accountId":own}),
            )
            .unwrap();
        let webview = fixture.webview.clone();
        let (completed, result) = std::sync::mpsc::channel();
        let publishing = std::thread::spawn(move || {
            completed
                .send(invoke(
                    &webview,
                    "save_account_bio",
                    serde_json::json!({"owner":"alice","bio":"Public profile"}),
                ))
                .unwrap();
        });
        let accepted = result.recv_timeout(std::time::Duration::from_secs(1));
        let responsive = fixture
            .invoke("owner_node_identity", serde_json::json!({"owner":"alice"}))
            .is_ok();
        release.store(true, Ordering::Release);
        stalled.join().unwrap();
        publishing.join().unwrap();
        assert!(
            accepted.is_ok_and(|value| value.is_ok()),
            "saving a locally committed profile must not wait for a peer handshake"
        );
        assert!(responsive, "profile fanout must preserve node observations");
    }

    #[test]
    fn registered_account_profile_commands_save_bio_without_accepting_identity_edits() {
        let fixture = Fixture::new();
        let own_id = fixture
            .node
            .0
            .blocking_read()
            .as_ref()
            .unwrap()
            .account_id()
            .to_owned();
        let get = || {
            fixture
                .invoke(
                    "get_account_profile",
                    serde_json::json!({"owner":"alice","accountId":own_id}),
                )
                .unwrap()
        };
        let initial = get();
        fixture.invoke("save_account_bio", serde_json::json!({"owner":"alice","bio":"Public bio","displayName":"Forged name","createdAtMs":42})).unwrap();
        let saved = get();
        assert_eq!(saved["bio"], "Public bio");
        assert_eq!(saved["display_name"], initial["display_name"]);
        assert_eq!(saved["created_at_ms"], initial["created_at_ms"]);
        for field in [
            "signed_json",
            "introductions",
            "witnesses",
            "revision",
            "avatar",
        ] {
            assert!(saved.get(field).is_none(), "retired field: {field}");
        }
        for command in [
            "get_trust_card",
            "publish_trust_card",
            "import_trust_artifact",
            "create_trust_introduction",
            "witness_trust_creation",
        ] {
            assert!(
                fixture
                    .invoke(command, serde_json::json!({"owner":"alice"}))
                    .is_err(),
                "retired command: {command}"
            );
        }
    }

    #[test]
    fn registered_account_profile_commands_reject_replaced_queued_authority() {
        use tauri::Manager;
        let fixture = Fixture::new();
        for change in ["relogin", "logout", "lock"] {
            for (command, body) in account_profile_command_cases("alice") {
                fixture.app_state.app_lock().clear();
                fixture
                    .app_state
                    .session()
                    .set("entry".into(), user("alice"), "pw".into());
                let (entered, captured) = std::sync::mpsc::channel();
                *fixture.app_state.owner_command_captured.lock().unwrap() = Some(entered);
                let lifecycle = fixture.node.0.blocking_write();
                let webview = fixture.webview.clone();
                let pending = std::thread::spawn(move || invoke(&webview, command, body));
                let captured = captured.recv_timeout(std::time::Duration::from_secs(2));
                match change {
                    "logout" => fixture.app_state.session().clear(),
                    "relogin" => fixture.app_state.session().set(
                        "replacement".into(),
                        user("alice"),
                        "pw".into(),
                    ),
                    _ => crate::app_lock::lock_app(
                        fixture.webview.state::<AppState>(),
                        fixture.webview.state::<crate::app_lock::AppLock>(),
                    )
                    .unwrap(),
                }
                drop(lifecycle);
                let result = pending.join().unwrap();
                assert!(
                    captured.is_ok(),
                    "profile commands must capture authority before waiting"
                );
                assert!(
                    matches!(result, Err(ref error) if error["kind"] == "authorization"),
                    "profile commands must reject retired authority after {change}"
                );
                *fixture.app_state.owner_command_captured.lock().unwrap() = None;
            }
        }
    }

    #[test]
    fn private_trust_commands_reject_uncertified_device_without_rewriting_history() {
        let fixture = Fixture::new();
        let trust_path =
            crate::config_store::account_path(&crate::data_dir(), "alice", "trust.json").unwrap();
        let before = std::fs::read(&trust_path).ok();
        for (command, body) in [
            (
                "get_trust",
                serde_json::json!({"owner":"alice","accountId":"claimed-account","currentFingerprint":"claimed-device"}),
            ),
            (
                "mark_verified",
                serde_json::json!({"owner":"alice","accountId":"claimed-account","fingerprint":"claimed-device"}),
            ),
        ] {
            assert_eq!(
                fixture.invoke(command, body).unwrap_err()["kind"],
                "authorization"
            );
            assert_eq!(std::fs::read(&trust_path).ok(), before);
        }
    }

    #[test]
    fn registered_plaintext_and_trust_commands_reject_replaced_entry_authority() {
        use tauri::Manager;
        let fixture = Fixture::new();
        let trust_path =
            crate::config_store::account_path(&crate::data_dir(), "alice", "trust.json").unwrap();
        let before = std::fs::read(&trust_path).ok();
        for change in ["relogin", "logout", "lock"] {
            for (command, body) in [
                ("search", serde_json::json!({"query":"private"})),
                ("read_file", serde_json::json!({"fileConv":"00".repeat(32)})),
                (
                    "read_media",
                    serde_json::json!({"fileConv":"00".repeat(32)}),
                ),
                (
                    "mark_verified",
                    serde_json::json!({"owner":"alice","accountId":"contact","fingerprint":"fp"}),
                ),
                (
                    "get_trust",
                    serde_json::json!({"owner":"alice","accountId":"contact","currentFingerprint":"fp"}),
                ),
            ] {
                fixture.app_state.app_lock().clear();
                fixture
                    .app_state
                    .session()
                    .set("entry".into(), user("alice"), "pw".into());
                let (entered, captured) = std::sync::mpsc::channel();
                *fixture.app_state.owner_command_captured.lock().unwrap() = Some(entered);
                let lifecycle = fixture.node.0.blocking_write();
                let webview = fixture.webview.clone();
                let pending = std::thread::spawn(move || invoke(&webview, command, body));
                let captured = captured.recv_timeout(std::time::Duration::from_secs(2));
                match change {
                    "logout" => fixture.app_state.session().clear(),
                    "relogin" => fixture.app_state.session().set(
                        "replacement".into(),
                        user("alice"),
                        "pw".into(),
                    ),
                    _ => crate::app_lock::lock_app(
                        fixture.webview.state::<AppState>(),
                        fixture.webview.state::<crate::app_lock::AppLock>(),
                    )
                    .unwrap(),
                }
                drop(lifecycle);
                let result = pending.join().unwrap();
                assert!(
                    captured.is_ok(),
                    "{command} must capture authority before waiting"
                );
                assert_eq!(
                    result.unwrap_err()["kind"],
                    "authorization",
                    "{command}: {change}"
                );
                assert_eq!(std::fs::read(&trust_path).ok(), before);
                *fixture.app_state.owner_command_captured.lock().unwrap() = None;
            }
        }
    }

    #[test]
    fn registered_commands_queued_behind_lifecycle_lock_reject_same_owner_new_generation() {
        let fixture = Fixture::new();
        let before = fixture.sync_documents();
        let cases = [
            (
                "get_device_sync_policy",
                serde_json::json!({"owner":"alice"}),
            ),
            (
                "get_device_sync_status",
                serde_json::json!({"owner":"alice"}),
            ),
            (
                "set_device_sync_category",
                serde_json::json!({"owner":"alice","category":"history", "enabled":false}),
            ),
            (
                "get_account_preferences",
                serde_json::json!({"owner":"alice","category":"appearance"}),
            ),
            (
                "set_account_preference",
                serde_json::json!({"owner":"alice","category":"appearance", "key":"theme", "value":"dark"}),
            ),
            (
                "send_file_to_account",
                serde_json::json!({"account":"11".repeat(16),"path":"unused","media":false}),
            ),
            (
                "send_to_account",
                serde_json::json!({"account":"11".repeat(16),"text":"queued","replyTo":null}),
            ),
            (
                "send_sticker",
                serde_json::json!({"convId":"11".repeat(16),"stickerId":"wave","fallback":"hello","isChannel":false}),
            ),
            ("owner_node_identity", serde_json::json!({"owner":"alice"})),
            (
                "owner_account_history",
                serde_json::json!({"owner":"alice","account":"11".repeat(16),"limit":10}),
            ),
            (
                "owner_delivery_statuses",
                serde_json::json!({"owner":"alice","account":"11".repeat(16),"ids":[]}),
            ),
            (
                "owner_enqueue_text",
                serde_json::json!({"owner":"alice","account":"11".repeat(16),"text":"queued","replyTo":null}),
            ),
            (
                "owner_enqueue_sticker",
                serde_json::json!({"owner":"alice","account":"11".repeat(16),"stickerId":"wave","fallback":"hello"}),
            ),
            (
                "owner_enqueue_file",
                serde_json::json!({"owner":"alice","account":"11".repeat(16),"path":"unused","media":false}),
            ),
        ];
        for (command, body) in cases {
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            *fixture.app_state.owner_command_captured.lock().unwrap() = Some(entered_tx);
            let lifecycle = fixture.node.0.blocking_write();
            let webview = fixture.webview.clone();
            let pending = std::thread::spawn(move || invoke(&webview, command, body));
            entered_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            fixture.app_state.session().clear();
            fixture
                .app_state
                .session()
                .set("new".into(), user("alice"), "pw".into());
            drop(lifecycle);
            let error = pending.join().unwrap().unwrap_err();
            assert_eq!(error["kind"], "authorization", "queued {command}");
            *fixture.app_state.owner_command_captured.lock().unwrap() = None;
        }
        let guard = fixture.node.0.blocking_write();
        let node = guard.as_ref().unwrap().handle();
        assert!(node.device_sync_policy().history);
        assert!(
            node.account_preferences(mesh_talk_core::node::SyncCategory::Appearance)
                .is_empty()
        );
        drop(guard);
        assert_eq!(fixture.sync_documents(), before);
    }

    #[test]
    fn registered_sync_mutations_queued_before_logout_or_lock_do_not_write() {
        use tauri::Manager;
        let fixture = Fixture::new();
        let before = fixture.sync_documents();
        let commands = [
            (
                "set_device_sync_category",
                serde_json::json!({"owner":"alice","category":"history", "enabled":false}),
            ),
            (
                "set_account_preference",
                serde_json::json!({"owner":"alice","category":"appearance", "key":"theme", "value":"dark"}),
            ),
        ];
        for lock in [false, true] {
            for (command, body) in &commands {
                fixture.app_state.app_lock().clear();
                fixture
                    .app_state
                    .session()
                    .set("a".into(), user("alice"), "pw".into());
                let (entered_tx, entered_rx) = std::sync::mpsc::channel();
                *fixture.app_state.owner_command_captured.lock().unwrap() = Some(entered_tx);
                let lifecycle = fixture.node.0.blocking_write();
                let webview = fixture.webview.clone();
                let command = *command;
                let body = body.clone();
                let pending = std::thread::spawn(move || invoke(&webview, command, body));
                entered_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                if lock {
                    crate::app_lock::lock_app(
                        fixture.webview.state::<AppState>(),
                        fixture.webview.state::<crate::app_lock::AppLock>(),
                    )
                    .unwrap();
                } else {
                    fixture.app_state.session().clear();
                }
                drop(lifecycle);
                let error = pending.join().unwrap().unwrap_err();
                assert_eq!(
                    error["kind"], "authorization",
                    "queued {command}, lock={lock}"
                );
                *fixture.app_state.owner_command_captured.lock().unwrap() = None;
            }
        }
        let guard = fixture.node.0.blocking_write();
        let node = guard.as_ref().unwrap().handle();
        assert!(node.device_sync_policy().history);
        assert!(
            node.account_preferences(mesh_talk_core::node::SyncCategory::Appearance)
                .is_empty()
        );
        drop(guard);
        assert_eq!(fixture.sync_documents(), before);
    }

    fn privacy_requests(account: &str) -> [(&'static str, serde_json::Value); 6] {
        [
            ("get_privacy", serde_json::json!({"owner":"alice"})),
            (
                "get_account_binding_changes",
                serde_json::json!({"owner":"alice","account":account}),
            ),
            (
                "accept_account_binding_change",
                serde_json::json!({"owner":"alice","change":{
                    "device_id":"a".repeat(32), "previous_account_id":"b".repeat(32), "current_account_id":account
                }}),
            ),
            (
                "set_invisible",
                serde_json::json!({"owner":"alice","invisible":true}),
            ),
            (
                "set_privacy_allowed",
                serde_json::json!({"owner":"alice","account":account,"allowed":true}),
            ),
            (
                "initiate_privacy_contact",
                serde_json::json!({"owner":"alice","account":account}),
            ),
        ]
    }

    fn privacy_documents(fixture: &Fixture, owner: &str) -> [Vec<u8>; 3] {
        ["privacy.policy", "peer-proofs", "peer-routes"].map(|name| {
            std::fs::read(fixture.root.path().join("accounts").join(owner).join(name)).unwrap()
        })
    }

    #[test]
    fn registered_privacy_commands_reject_app_lock_and_stale_installation_without_writes() {
        use tauri::Manager;
        for locked in [true, false] {
            let fixture = Fixture::new();
            let account = fixture.offline_peer();
            fixture
                .node
                .0
                .blocking_write()
                .as_ref()
                .unwrap()
                .handle()
                .cached_peer_snapshot()
                .unwrap();
            let before = privacy_documents(&fixture, "alice");
            if locked {
                crate::app_lock::lock_app(
                    fixture.webview.state::<AppState>(),
                    fixture.webview.state::<crate::app_lock::AppLock>(),
                )
                .unwrap();
            } else {
                fixture.node.next_lifecycle_ticket();
            }
            for (command, body) in privacy_requests(&account) {
                assert!(
                    fixture.invoke(command, body).is_err(),
                    "{command} accepted locked={locked}"
                );
                assert_eq!(
                    privacy_documents(&fixture, "alice"),
                    before,
                    "{command} wrote locked={locked}"
                );
            }
        }
    }

    #[test]
    fn registered_privacy_commands_queued_before_same_owner_generation_change_do_not_write() {
        let fixture = Fixture::new();
        let account = fixture.offline_peer();
        fixture
            .node
            .0
            .blocking_write()
            .as_ref()
            .unwrap()
            .handle()
            .cached_peer_snapshot()
            .unwrap();
        let before = privacy_documents(&fixture, "alice");
        for (command, body) in privacy_requests(&account) {
            let (entered, captured) = std::sync::mpsc::channel();
            *fixture.app_state.owner_command_captured.lock().unwrap() = Some(entered);
            let lifecycle = fixture.node.0.blocking_write();
            let webview = fixture.webview.clone();
            let pending = std::thread::spawn(move || invoke(&webview, command, body));
            captured
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            fixture
                .app_state
                .session()
                .set("replacement".into(), user("alice"), "pw".into());
            drop(lifecycle);
            assert!(
                pending.join().unwrap().is_err(),
                "{command} accepted old generation"
            );
            assert_eq!(
                privacy_documents(&fixture, "alice"),
                before,
                "{command} wrote old generation"
            );
            *fixture.app_state.owner_command_captured.lock().unwrap() = None;
        }
    }

    #[test]
    fn registered_privacy_mutations_reject_logout_while_waiting_for_policy_gate() {
        for command in [
            "set_invisible",
            "set_privacy_allowed",
            "initiate_privacy_contact",
        ] {
            registered_logout_during_local_grant_wait(command);
        }
    }

    fn registered_logout_during_local_grant_wait(command: &'static str) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        // Keep only one deliberately blocked legacy callback on Tauri's shared
        // IPC pool, leaving registered logout runnable while the gate is held.
        // Ordinary tests and product work remain concurrent.
        static GRANT_WAIT_PROBES: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _probe = GRANT_WAIT_PROBES.lock().unwrap();
        let fixture = Fixture::create(true);
        let lease = fixture.app_state.session().capture().unwrap();
        let owner = lease.owner().to_owned();
        let account = fixture.offline_peer();
        let node = fixture.node.0.blocking_write().as_ref().unwrap().handle();
        node.cached_peer_snapshot().unwrap();
        let directory = fixture.root.path().join("accounts").join(&owner);
        let metadata = || {
            ["privacy.policy", "peer-proofs", "peer-routes"]
                .map(|name| std::fs::read(directory.join(name)).unwrap())
        };
        let before = metadata();
        let policy = node.privacy_snapshot();
        // Discovery may publish our signed profile while this probe waits. Check
        // every other event kind without mistaking those independent public
        // profile writes for work accepted from the rejected owner command.
        let event_ids = || {
            let snapshot = tempfile::tempdir().unwrap();
            let path = snapshot.path().join("messages.log");
            std::fs::copy(directory.join("messages.log"), &path).unwrap();
            let log = mesh_talk_core::eventlog::persist::PersistentEventLog::open(
                &path,
                "password-owner",
            )
            .unwrap();
            let mut profiles = 0;
            let other = log
                .all_event_ids()
                .into_iter()
                .filter(|id| {
                    let profile = log.get(id).unwrap().kind
                        == mesh_talk_core::eventlog::event::EventKind::Profile;
                    profiles += usize::from(profile);
                    !profile
                })
                .collect::<std::collections::BTreeSet<_>>();
            (other, profiles)
        };
        let (before_log, before_profiles) = event_ids();
        let work = || {
            [
                "sent.log",
                "ratchet.sessions",
                "received_files.log",
                "delivery-transactions.log",
                "delivery-outbox.log",
            ]
            .map(|name| match std::fs::read(directory.join(name)) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => panic!("producer snapshot read failed: {error}"),
            })
        };
        let before_work = work();
        let path = fixture.root.path().join("unstaged.txt");
        std::fs::write(&path, b"owner already lost before local grant").unwrap();
        let staged = Arc::new(AtomicUsize::new(0));
        let produced = staged.clone();
        *fixture.app_state.owner_file_progress_hook.lock().unwrap() = Some(Box::new(move || {
            produced.fetch_add(1, Ordering::SeqCst);
        }));
        // Public legacy authorization owns the actual serialized privacy gate.
        // Its own mutation is rejected when released, isolating the enqueue producer.
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let holder_node = node.clone();
        let holder_account = account.clone();
        let holder = std::thread::spawn(move || {
            tauri::async_runtime::block_on(holder_node.set_allowed_if(
                &holder_account,
                false,
                move || {
                    held_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "test gate holder performs no mutation",
                    ))
                },
            ))
        });
        held_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        *fixture.app_state.owner_command_captured.lock().unwrap() = Some(entered_tx);
        let body = match command {
            "set_invisible" => serde_json::json!({"owner":owner,"invisible":true}),
            "set_privacy_allowed" => {
                serde_json::json!({"owner":owner,"account":account,"allowed":true})
            }
            "initiate_privacy_contact" => serde_json::json!({"owner":owner,"account":account}),
            "send_file_to_account" => {
                serde_json::json!({"account":account,"path":path,"media":false})
            }
            "send_to_account" => {
                serde_json::json!({"account":account,"text":"pending grant","replyTo":null})
            }
            "send_sticker" => {
                serde_json::json!({"convId":account,"stickerId":"wave","fallback":"hello","isChannel":false})
            }
            "owner_enqueue_text" => {
                serde_json::json!({"owner":owner,"account":account,"text":"pending grant","replyTo":null})
            }
            "owner_enqueue_sticker" => {
                serde_json::json!({"owner":owner,"account":account,"stickerId":"wave","fallback":"hello"})
            }
            "owner_enqueue_file" => {
                serde_json::json!({"owner":owner,"account":account,"path":path,"media":false})
            }
            _ => unreachable!(),
        };
        let webview = fixture.webview.clone();
        let pending = std::thread::spawn(move || invoke(&webview, command, body));
        let wait_phase = |phase| loop {
            match entered_rx.recv_timeout(std::time::Duration::from_secs(5)) {
                Ok(actual) if actual == phase => return true,
                Ok(_) => continue,
                Err(_) => return false,
            }
        };
        let privacy = matches!(
            command,
            "set_invisible" | "set_privacy_allowed" | "initiate_privacy_contact"
        );
        let authorized = wait_phase(if privacy {
            "owner-entry"
        } else {
            "owner-authorized"
        });
        if privacy {
            // Capture precedes the runtime mutex. Wait until the detached IPC
            // owns that mutex and is queued behind the actual core policy gate.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while fixture.node.0.try_write().is_ok() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "privacy never retained lifecycle"
                );
                std::thread::yield_now();
            }
        }
        let webview = fixture.webview.clone();
        let logout = std::thread::spawn(move || invoke(&webview, "logout", serde_json::json!({})));
        let published = wait_phase("logout-published");
        let logged_out = fixture.app_state.session().get().is_none();
        // Real registered logout publication above; direct authenticated re-publication
        // below models the same host UUID's replacement generation, not runtime startup.
        let (_, _, replacement) = fixture
            .app_state
            .auth_service()
            .login_session("alice".into(), "password-owner".into())
            .unwrap();
        let replaced = replacement.owner() == owner && replacement != lease;
        release_tx.send(()).unwrap();
        let holder_result = holder.join().unwrap();
        let result = pending.join().unwrap();
        let logout_result = logout.join().unwrap();
        *fixture.app_state.owner_command_captured.lock().unwrap() = None;
        let metadata_unchanged = metadata() == before;
        let policy_unchanged = node.privacy_snapshot() == policy;
        let (after_log, after_profiles) = event_ids();
        let non_profile_log_unchanged = after_log == before_log;
        let staged = staged.load(Ordering::SeqCst);
        let history_empty = node.account_history(&account, 10).is_empty();
        let work_unchanged = work() == before_work;
        drop(node);
        // The registered logout child has completed its real retirement barrier.
        // Reopen exactly the same owner profile; no late producer may repair/hide state.
        let reopened = tauri::async_runtime::block_on(mesh_talk_core::node::NodeRuntime::start(
            fixture.root.path(),
            &owner,
            "Alice",
            "password-owner",
            fixture.discovery_port,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        ))
        .unwrap();
        let reopened_policy = reopened.handle().privacy_snapshot();
        let reopened_empty = reopened.account_history(&account, 10).is_empty();
        tauri::async_runtime::block_on(reopened.stop());
        eprintln!(
            "{command}: initial_authorized={authorized}, logout_published={published}, same_uuid_replaced={replaced}, metadata_unchanged={metadata_unchanged}, policy_unchanged={policy_unchanged}, staged_callbacks={staged}, non_profile_log_unchanged={non_profile_log_unchanged}, profiles_before={before_profiles}, profiles_after={after_profiles}"
        );
        assert!(authorized && published && logged_out && replaced);
        assert_eq!(
            holder_result.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(logout_result.unwrap(), serde_json::json!({"success":true}));
        let error = result.unwrap_err();
        if privacy {
            assert_eq!(error, serde_json::json!("Could not update network privacy"));
        } else {
            assert_eq!(error["kind"], "authorization");
        }
        assert!(history_empty);
        assert!(reopened_empty && reopened_policy == policy);
        assert!(
            work_unchanged,
            "rejected owner command must not append local message/file work"
        );
        assert!(
            metadata_unchanged && policy_unchanged,
            "rejected registered {command} must not persist a grant after logout publication"
        );
        assert_eq!(staged, 0);
        assert!(non_profile_log_unchanged);
    }

    #[test]
    fn registered_legacy_file_logout_while_grant_waits_rejects_without_side_effects() {
        registered_logout_during_local_grant_wait("send_file_to_account");
    }

    #[test]
    fn registered_legacy_text_logout_while_grant_waits_rejects_without_side_effects() {
        registered_logout_during_local_grant_wait("send_to_account");
    }
    #[test]
    fn registered_legacy_sticker_logout_while_grant_waits_rejects_without_side_effects() {
        registered_logout_during_local_grant_wait("send_sticker");
    }

    #[test]
    fn registered_logout_while_grant_waits_rejects_text_without_permission_side_effects() {
        registered_logout_during_local_grant_wait("owner_enqueue_text");
    }
    #[test]
    fn registered_logout_while_grant_waits_rejects_sticker_without_permission_side_effects() {
        registered_logout_during_local_grant_wait("owner_enqueue_sticker");
    }
    #[test]
    fn registered_logout_while_grant_waits_rejects_file_without_permission_or_staging() {
        registered_logout_during_local_grant_wait("owner_enqueue_file");
    }

    #[test]
    fn cancelled_host_file_request_retains_lifecycle_guard_until_actual_staging_finishes() {
        use tauri::Manager;
        let fixture = Fixture::new();
        let account = fixture.offline_peer();
        let path = fixture.root.path().join("cancel-stage.txt");
        std::fs::write(&path, b"real file chunk before pause").unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = std::sync::Mutex::new(release_rx);
        *fixture.app_state.owner_file_progress_hook.lock().unwrap() = Some(Box::new(move || {
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
        }));
        let handle = fixture.webview.app_handle().clone();
        let target = account.clone();
        let request = tauri::async_runtime::spawn(async move {
            super::owner_enqueue_file(
                "alice".into(),
                target,
                path.to_string_lossy().into_owned(),
                false,
                None,
                None,
                None,
                handle.clone(),
                handle.state::<AppState>(),
                handle.state::<NodeState>(),
            )
            .await
        });
        tauri::async_runtime::block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), entered_rx)
                .await
                .unwrap()
                .unwrap();
            // This gate is the real core progress callback after an actual chunk
            // append, inside the blocking stage writer; no mocked writer task.
            request.abort();
            assert!(request.await.is_err());
            assert!(
                fixture.node.0.try_write().is_err(),
                "dropping IPC must not detach the file writer from lifecycle admission"
            );
            fixture.app_state.session().clear();
            fixture
                .app_state
                .session()
                .set("new".into(), user("bob"), "pw".into());
            release_tx.send(()).unwrap();
            let mut guard = fixture.node.0.write().await;
            let old = guard.take().unwrap();
            assert!(
                old.account_history(&account, 10).is_empty(),
                "changed owner must reject the staged card before WAL acceptance"
            );
            drop(guard);
            old.stop().await;
            let replacement = mesh_talk_core::node::NodeRuntime::start(
                fixture.root.path(),
                "bob",
                "Bob",
                "pw",
                0,
                |_| {},
                |_| {},
                |_| {},
                |_| {},
                |_| {},
            )
            .await
            .unwrap();
            *fixture.node.0.write().await = Some(replacement);
        });
        let identity = fixture
            .invoke("owner_node_identity", serde_json::json!({"owner":"bob"}))
            .unwrap();
        assert_eq!(identity["owner"], "bob");
    }

    #[test]
    fn registered_identity_query_returns_actual_matching_runtime_and_rejects_replacement_session() {
        let root = tempfile::tempdir().unwrap();
        let app_state = AppState::new(AuthService::new(Arc::new(IdentityManager::new(
            FileManager::new(root.path().to_owned()),
        ))));
        let node = NodeState::empty();
        let app = tauri::test::mock_builder()
            .manage(app_state.clone())
            .manage(node.clone())
            .invoke_handler(tauri::generate_handler![super::owner_node_identity])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let invoke = |owner: &str| {
            tauri::test::get_ipc_response(
                &webview,
                tauri::webview::InvokeRequest {
                    cmd: "owner_node_identity".into(),
                    callback: tauri::ipc::CallbackFn(0),
                    error: tauri::ipc::CallbackFn(1),
                    url: if cfg!(windows) {
                        "http://tauri.localhost"
                    } else {
                        "tauri://localhost"
                    }
                    .parse()
                    .unwrap(),
                    body: tauri::ipc::InvokeBody::Json(serde_json::json!({"owner": owner})),
                    headers: Default::default(),
                    invoke_key: tauri::test::INVOKE_KEY.into(),
                },
            )
            .map(|body| body.deserialize::<serde_json::Value>().unwrap())
        };
        app_state
            .session()
            .set("a".into(), user("alice"), "pw".into());
        tauri::async_runtime::block_on(async {
            let rt = mesh_talk_core::node::NodeRuntime::start(
                root.path(),
                "alice",
                "Alice",
                "pw",
                0,
                |_| {},
                |_| {},
                |_| {},
                |_| {},
                |_| {},
            )
            .await
            .unwrap();
            *node.0.write().await = Some(rt);
        });
        let identity = invoke("alice").expect(
            "owner-sensitive identity command is registered and returns the actual runtime",
        );
        assert_eq!(identity["owner"], "alice");
        assert_eq!(identity["history_revision"], 0);
        assert!(identity["device_id"].as_str().unwrap().len() == 32);
        app_state
            .session()
            .set("b".into(), user("bob"), "pw".into());
        assert!(invoke("alice").is_err());
        assert!(
            invoke("bob").is_err(),
            "Session B must never observe still-installed runtime A"
        );
        tauri::async_runtime::block_on(async {
            node.0.write().await.take().unwrap().stop().await;
        });
    }
    #[test]
    fn delivery_observation_is_optional_and_additive() {
        let base = super::MessageStatus {
            id: "message".into(),
            status: mesh_talk_core::node::DeliveryStatus::Awaiting,
            observation: None,
            file_progress: None,
        };
        assert_eq!(
            serde_json::to_value(&base).unwrap(),
            serde_json::json!({"id":"message", "status":"awaiting"})
        );
        let observed = super::MessageStatus {
            observation: Some(mesh_talk_core::node::DeliveryObservation {
                kind: mesh_talk_core::node::DeliveryObservationKind::RetryScheduled,
                age_ms: 42,
                reason: None,
            }),
            ..base
        };
        assert_eq!(
            serde_json::to_value(&observed).unwrap(),
            serde_json::json!({"id":"message", "status":"awaiting", "observation":{"kind":"retry_scheduled", "age_ms":42}})
        );
        let explained = super::MessageStatus {
            observation: Some(mesh_talk_core::node::DeliveryObservation {
                kind: mesh_talk_core::node::DeliveryObservationKind::RetryScheduled,
                age_ms: 42,
                reason: Some(mesh_talk_core::node::DeliveryRecoveryReason::NoKnownRoute),
            }),
            ..observed
        };
        assert_eq!(
            serde_json::to_value(&explained).unwrap()["observation"]["reason"],
            "no_known_route"
        );
    }
}
