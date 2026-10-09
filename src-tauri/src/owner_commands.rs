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
use std::{future::Future, pin::Pin};

type OwnerFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, CommandError>> + Send + 'a>>;

/// The owner captured at IPC entry, together with the only authority allowed to
/// run synchronous producers against the installed runtime. Detached enqueues
/// retain the lifecycle guard even if the caller cancels its IPC future.
struct OwnerAdmission {
    app: AppState,
    node: NodeState,
    lease: SessionLease,
}

impl OwnerAdmission {
    fn capture(app: &AppState, node: &NodeState, owner: &str) -> Result<Self, CommandError> {
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
        Ok(Self {
            app: app.clone(),
            node: node.clone(),
            lease,
        })
    }

    fn authorized<T>(
        &self,
        runtime: &NodeRuntime,
        operation: impl FnOnce() -> Result<T, CommandError>,
    ) -> Result<T, CommandError> {
        self.app
            .session()
            .matching(&self.lease, |_| {
                if runtime.host_account_id() != Some(self.lease.owner()) {
                    return Err(CommandError::Authorization("runtime owner mismatch".into()));
                }
                self.node.check_installation(&self.lease)?;
                operation()
            })
            .map_err(CommandError::Authorization)?
    }

    async fn read<T>(
        &self,
        operation: impl FnOnce(&NodeRuntime) -> Result<T, CommandError>,
    ) -> Result<T, CommandError> {
        let guard = self.node.0.lock().await;
        let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
        self.authorized(runtime, || operation(runtime))
    }

    fn authorize_accept(
        &self,
        runtime: &NodeRuntime,
        accept: &mut dyn FnMut() -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        // The session guard encloses each synchronous producer (local grant and WAL),
        // not just a preceding check. No session guard crosses an async policy wait.
        let result = self
            .authorized(runtime, || Ok(accept()))
            .map_err(|error| NodeError::Authorization(error.to_string()))?;
        #[cfg(test)]
        if result.is_ok() {
            if let Some(entered) = self.app.owner_command_captured.lock().unwrap().as_ref() {
                let _ = entered.send("owner-authorized");
            }
        }
        result
    }

    async fn detached<T: Send + 'static>(
        self,
        operation: impl for<'a> FnOnce(&'a NodeRuntime, &'a Self) -> OwnerFuture<'a, T> + Send + 'static,
    ) -> Result<T, CommandError> {
        let guard = self.node.0.clone().lock_owned().await;
        tokio::spawn(async move {
            let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
            operation(runtime, &self).await
        })
        .await
        .map_err(|_| CommandError::Internal("enqueue operation terminated".into()))?
    }
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
    let admission = OwnerAdmission::capture(&app, &node, &owner)?;
    admission
        .read(|runtime| {
            Ok(NodeIdentity {
                owner,
                device_id: runtime.user_id().into(),
                account_id: runtime.account_id().into(),
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
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<String, CommandError> {
    let admission = OwnerAdmission::capture(&app, &node, &owner)?;
    validate_account(&account)?;
    let reply = reply_to.as_deref().map(parse_id).transpose()?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let id = runtime
                    .handle()
                    .enqueue_to_account_if(&account, text.as_bytes(), reply, |accept| {
                        admission.authorize_accept(runtime, accept)
                    })
                    .await
                    .map_err(CommandError::from)?;
                Ok(hex::encode(id.as_bytes()))
            })
        })
        .await
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
    let admission = OwnerAdmission::capture(&app, &node, &owner)?;
    validate_account(&account)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let id = runtime
                    .handle()
                    .enqueue_sticker_to_account_if(
                        &account,
                        &sticker_id,
                        fallback.as_bytes(),
                        |accept| admission.authorize_accept(runtime, accept),
                    )
                    .await
                    .map_err(CommandError::from)?;
                Ok(hex::encode(id.as_bytes()))
            })
        })
        .await
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
    let admission = OwnerAdmission::capture(&app, &node, &owner)?;
    validate_account(&account)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let kind = if media {
                    mesh_talk_core::file::FileKind::Media
                } else {
                    mesh_talk_core::file::FileKind::File
                };
                #[cfg(test)]
                let progress_hook = admission.app.owner_file_progress_hook.clone();
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
                        |accept| admission.authorize_accept(runtime, accept),
                    )
                    .await
                    .map_err(CommandError::from)?;
                Ok(AcceptedFile {
                    id: hex::encode(id.as_bytes()),
                    file_conv: hex::encode(conv.as_bytes()),
                })
            })
        })
        .await
}

#[tauri::command]
pub async fn owner_account_history(
    owner: String,
    account: String,
    limit: usize,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<Vec<HistoryItem>, CommandError> {
    let admission = OwnerAdmission::capture(&app, &node, &owner)?;
    validate_account(&account)?;
    admission
        .read(|runtime| {
            Ok(runtime
                .account_history(&account, limit.min(500))
                .into_iter()
                .map(HistoryItem::from)
                .collect())
        })
        .await
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
    let admission = OwnerAdmission::capture(&app, &node, &owner)?;
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
        .read(|runtime| {
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
            // These probes deliberately hold a synchronous privacy-state writer.
            // Background readers can block indefinitely until the test releases it;
            // isolate them so low-core runners can still execute registered logout.
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
                let (user, token) = app_state
                    .auth_service()
                    .login("alice".into(), password.into())
                    .unwrap();
                let owner = user.user_id.clone();
                app_state.session().publish(token, user, password.into());
                owner
            } else {
                app_state
                    .session()
                    .set("a".into(), user("alice"), "pw".into());
                "alice".into()
            };
            let node = NodeState::empty();
            let app = tauri::test::mock_builder()
                .manage(app_state.clone())
                .manage(node.clone())
                .manage(crate::settings::SettingsState::isolated(
                    root.path().join("settings.json"),
                ))
                .invoke_handler(tauri::generate_handler![
                    super::owner_node_identity,
                    super::owner_enqueue_text,
                    super::owner_enqueue_sticker,
                    super::owner_enqueue_file,
                    super::owner_account_history,
                    super::owner_delivery_statuses,
                    crate::commands::logout
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
            *node.0.blocking_lock() = Some(runtime);
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

    fn registered_logout_during_local_grant_wait(command: &'static str) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        // Routing also consults the held state synchronously. Keep only one
        // deliberate blocker on Tauri's shared IPC pool, leaving logout runnable.
        // Ordinary tests and product work remain concurrent.
        static GRANT_WAIT_PROBES: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _probe = GRANT_WAIT_PROBES.lock().unwrap();
        let fixture = Fixture::create(true);
        let lease = fixture.app_state.session().capture().unwrap();
        let owner = lease.owner().to_owned();
        let account = fixture.offline_peer();
        let node = fixture.node.0.blocking_lock().as_ref().unwrap().handle();
        node.cached_peer_snapshot().unwrap();
        let directory = fixture.root.path().join("accounts").join(&owner);
        let metadata = || {
            ["privacy.policy", "peer-proofs", "peer-routes"]
                .map(|name| std::fs::read(directory.join(name)).unwrap())
        };
        let before = metadata();
        let policy = node.privacy_snapshot();
        let before_log = std::fs::read(directory.join("messages.log")).unwrap();
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
        // Public legacy authorization owns the actual privacy gate and state lock.
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
        let authorized = wait_phase("owner-authorized");
        let webview = fixture.webview.clone();
        let logout = std::thread::spawn(move || invoke(&webview, "logout", serde_json::json!({})));
        let published = wait_phase("logout-published");
        let logged_out = fixture.app_state.session().get().is_none();
        // Real registered logout publication above; direct authenticated re-publication
        // below models the same host UUID's replacement generation, not runtime startup.
        let (user, token) = fixture
            .app_state
            .auth_service()
            .login("alice".into(), "password-owner".into())
            .unwrap();
        let replacement = fixture
            .app_state
            .session()
            .publish(token, user, "password-owner".into());
        let replaced = replacement.owner() == owner && replacement != lease;
        release_tx.send(()).unwrap();
        let holder_result = holder.join().unwrap();
        let result = pending.join().unwrap();
        let logout_result = logout.join().unwrap();
        *fixture.app_state.owner_command_captured.lock().unwrap() = None;
        let metadata_unchanged = metadata() == before;
        let policy_unchanged = node.privacy_snapshot() == policy;
        let log_unchanged = std::fs::read(directory.join("messages.log")).unwrap() == before_log;
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
        eprintln!("{command}: initial_authorized={authorized}, logout_published={published}, same_uuid_replaced={replaced}, metadata_unchanged={metadata_unchanged}, policy_unchanged={policy_unchanged}, staged_callbacks={staged}, log_unchanged={log_unchanged}");
        assert!(authorized && published && logged_out && replaced);
        assert_eq!(
            holder_result.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(logout_result.unwrap(), serde_json::json!({"success":true}));
        assert_eq!(result.unwrap_err()["kind"], "authorization");
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
        assert!(log_unchanged);
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
