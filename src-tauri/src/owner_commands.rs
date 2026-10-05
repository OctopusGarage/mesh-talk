//! Owner-sensitive automatic delivery IPC. Legacy commands retain their shapes.

use crate::{
    chat_commands::{HistoryItem, NodeState},
    commands::CommandError,
    state::{AppState, SessionLease},
};
use mesh_talk_core::{
    eventlog::EventId,
    node::{NodeError, NodeRuntime},
};
use serde::Serialize;

fn capture(app: &AppState, owner: &str) -> Result<SessionLease, CommandError> {
    let lease = app
        .session()
        .capture()
        .map_err(CommandError::Authorization)?;
    if lease.owner() != owner {
        return Err(CommandError::Authorization("session owner mismatch".into()));
    }
    #[cfg(test)]
    if let Some(entered) = app.owner_command_captured.lock().unwrap().as_ref() {
        let _ = entered.send("owner-entry");
    }
    Ok(lease)
}

fn authorized<T>(
    app: &AppState,
    state: &NodeState,
    lease: &SessionLease,
    runtime: &NodeRuntime,
    operation: impl FnOnce() -> Result<T, CommandError>,
) -> Result<T, CommandError> {
    app.session()
        .matching(lease, |_| {
            if runtime.host_account_id() != Some(lease.owner()) {
                return Err(CommandError::Authorization("runtime owner mismatch".into()));
            }
            state.check_installation(lease)?;
            operation()
        })
        .map_err(CommandError::Authorization)?
}

fn authorize_accept(
    app: &AppState,
    state: &NodeState,
    lease: &SessionLease,
    runtime: &NodeRuntime,
    accept: &mut dyn FnMut() -> Result<(), NodeError>,
) -> Result<(), NodeError> {
    // The session guard encloses the WAL append itself, not just a preceding check.
    authorized(app, state, lease, runtime, || Ok(accept()))
        .map_err(|error| NodeError::Authorization(error.to_string()))?
}

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
    device_id: String,
    account_id: String,
}

/// Atomically observes Session + runtime identity, including installation authority.
#[tauri::command]
pub async fn owner_node_identity(
    owner: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<NodeIdentity, CommandError> {
    let lease = capture(&app, &owner)?;
    let guard = node.0.lock().await;
    let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
    authorized(&app, &node, &lease, runtime, || {
        Ok(NodeIdentity {
            owner,
            device_id: runtime.user_id().into(),
            account_id: runtime.account_id().into(),
        })
    })
}

#[tauri::command]
pub async fn owner_enqueue_text(
    owner: String,
    account: String,
    text: String,
    reply_to: Option<String>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<String, CommandError> {
    let lease = capture(&app, &owner)?;
    validate_account(&account)?;
    let reply = reply_to.as_deref().map(parse_id).transpose()?;
    let app = app.inner().clone();
    let node = node.inner().clone();
    let guard = node.0.clone().lock_owned().await;
    tokio::spawn(async move {
        let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
        let id = runtime
            .handle()
            .enqueue_to_account_if(&account, text.as_bytes(), reply, |accept| {
                authorize_accept(&app, &node, &lease, runtime, accept)
            })
            .await
            .map_err(CommandError::from)?;
        Ok(hex::encode(id.as_bytes()))
    })
    .await
    .map_err(|_| CommandError::Internal("enqueue operation terminated".into()))?
}

#[tauri::command]
pub async fn owner_enqueue_sticker(
    owner: String,
    account: String,
    sticker_id: String,
    fallback: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<String, CommandError> {
    let lease = capture(&app, &owner)?;
    validate_account(&account)?;
    let app = app.inner().clone();
    let node = node.inner().clone();
    let guard = node.0.clone().lock_owned().await;
    tokio::spawn(async move {
        let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
        let id = runtime
            .handle()
            .enqueue_sticker_to_account_if(&account, &sticker_id, fallback.as_bytes(), |accept| {
                authorize_accept(&app, &node, &lease, runtime, accept)
            })
            .await
            .map_err(CommandError::from)?;
        Ok(hex::encode(id.as_bytes()))
    })
    .await
    .map_err(|_| CommandError::Internal("enqueue operation terminated".into()))?
}

#[derive(Serialize)]
pub struct AcceptedFile {
    id: String,
    #[serde(rename = "fileConv")]
    file_conv: String,
}

#[tauri::command]
pub async fn owner_enqueue_file(
    owner: String,
    account: String,
    path: String,
    media: bool,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<AcceptedFile, CommandError> {
    let lease = capture(&app, &owner)?;
    validate_account(&account)?;
    let app = app.inner().clone();
    let node = node.inner().clone();
    // The admitted task owns this lifecycle guard through blocking staging,
    // even if the frontend drops its IPC future while the file writer runs.
    let guard = node.0.clone().lock_owned().await;
    tokio::spawn(async move {
        let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
        let kind = if media {
            mesh_talk_core::file::FileKind::Media
        } else {
            mesh_talk_core::file::FileKind::File
        };
        #[cfg(test)]
        let progress_hook = app.owner_file_progress_hook.clone();
        let (id, conv) = runtime
            .handle()
            .enqueue_file_to_account_progress_if(
                &account,
                std::path::Path::new(&path),
                kind,
                move |_| {
                    #[cfg(test)]
                    {
                        let hook = progress_hook.lock().unwrap().take();
                        if let Some(hook) = hook {
                            hook();
                        }
                    }
                },
                |accept| authorize_accept(&app, &node, &lease, runtime, accept),
            )
            .await
            .map_err(CommandError::from)?;
        Ok(AcceptedFile {
            id: hex::encode(id.as_bytes()),
            file_conv: hex::encode(conv.as_bytes()),
        })
    })
    .await
    .map_err(|_| CommandError::Internal("enqueue operation terminated".into()))?
}

#[tauri::command]
pub async fn owner_account_history(
    owner: String,
    account: String,
    limit: usize,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<Vec<HistoryItem>, CommandError> {
    let lease = capture(&app, &owner)?;
    validate_account(&account)?;
    let guard = node.0.lock().await;
    let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
    authorized(&app, &node, &lease, runtime, || {
        Ok(runtime
            .account_history(&account, limit.min(500))
            .into_iter()
            .map(HistoryItem::from)
            .collect())
    })
}

#[derive(Serialize)]
pub struct MessageStatus {
    id: String,
    status: mesh_talk_core::node::DeliveryStatus,
}

#[tauri::command]
pub async fn owner_delivery_statuses(
    owner: String,
    account: String,
    ids: Vec<String>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<Vec<MessageStatus>, CommandError> {
    let lease = capture(&app, &owner)?;
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
    let guard = node.0.lock().await;
    let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
    authorized(&app, &node, &lease, runtime, || {
        Ok(runtime
            .handle()
            .account_delivery_statuses(&account, &ids)
            .into_iter()
            .map(|(id, status)| MessageStatus {
                id: hex::encode(id.as_bytes()),
                status,
            })
            .collect())
    })
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
            name: owner.into(),
            display_name: owner.into(),
            address: "fixture".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        }
    }

    struct Fixture {
        root: tempfile::TempDir,
        app_state: AppState,
        node: NodeState,
        webview: tauri::WebviewWindow<tauri::test::MockRuntime>,
        discovery_port: u16,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let app_state = AppState::new(AuthService::new(Arc::new(IdentityManager::new(
                FileManager::new(root.path().to_owned()),
            ))));
            app_state
                .session()
                .set("a".into(), user("alice"), "pw".into());
            let node = NodeState::empty();
            let app = tauri::test::mock_builder()
                .manage(app_state.clone())
                .manage(node.clone())
                .invoke_handler(tauri::generate_handler![
                    super::owner_node_identity,
                    super::owner_enqueue_text,
                    super::owner_enqueue_sticker,
                    super::owner_enqueue_file,
                    super::owner_account_history,
                    super::owner_delivery_statuses
                ])
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap();
            let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
                .build()
                .unwrap();
            let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            let discovery_port = socket.local_addr().unwrap().port();
            drop(socket);
            let runtime = tauri::async_runtime::block_on(mesh_talk_core::node::NodeRuntime::start(
                root.path(),
                "alice",
                "Alice",
                "pw",
                discovery_port,
                |_| {},
                |_| {},
                |_| {},
                |_| {},
                |_| {},
            ))
            .unwrap();
            *node.0.blocking_lock() = Some(runtime);
            Self {
                root,
                app_state,
                node,
                webview,
                discovery_port,
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
                        .lock()
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
            let guard = fixture.node.0.lock().await;
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
        let reservation = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
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
        fixture.announce_peer(
            remote
                .handle()
                .signed_announce("Sender", remote.listen_tcp_port()),
        );
        let (announce, account, device) = {
            let guard = fixture.node.0.blocking_lock();
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
                    .lock()
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
            let runtime = self.node.0.blocking_lock().take();
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
        assert!(statuses
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["status"] == "awaiting"));
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
        assert!(fixture
            .invoke(
                "owner_delivery_statuses",
                serde_json::json!({"owner":"alice", "account":account, "ids":ids})
            )
            .is_err());
    }

    #[test]
    fn registered_commands_queued_behind_lifecycle_lock_reject_same_owner_new_generation() {
        let fixture = Fixture::new();
        let cases = [
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
            let lifecycle = fixture.node.0.blocking_lock();
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
                fixture.node.0.try_lock().is_err(),
                "dropping IPC must not detach the file writer from lifecycle admission"
            );
            fixture.app_state.session().clear();
            fixture
                .app_state
                .session()
                .set("new".into(), user("bob"), "pw".into());
            release_tx.send(()).unwrap();
            let mut guard = fixture.node.0.lock().await;
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
            *fixture.node.0.lock().await = Some(replacement);
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
            *node.0.lock().await = Some(rt);
        });
        let identity = invoke("alice").expect(
            "owner-sensitive identity command is registered and returns the actual runtime",
        );
        assert_eq!(identity["owner"], "alice");
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
            node.0.lock().await.take().unwrap().stop().await;
        });
    }
}
