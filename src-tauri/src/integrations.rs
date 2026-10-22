//! Session-bound desktop host for the local integrations service.

use crate::{
    chat_commands::NodeState,
    runtime_admission::RuntimeAdmission,
    state::{AppState, SessionLease},
};
use mesh_talk_core::{
    eventlog::event::EventId,
    node::{MessageAcceptance, MessageDestination, NodeRuntime, OutgoingMessageRequest},
};
use mesh_talk_integrations::{
    IntegrationError, IntegrationService,
    client::{ClientConfig, EndpointManifest, endpoint_reference},
    ipc::LocalServer,
    policy::{GrantRegistry, new_token},
    private_files::{replace_private_json, write_private_json},
    service::{CommitAuthorization, HostFuture, HostOperation, IntegrationHost},
};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::mpsc;

type FlushRequest = (MessageDestination, EventId);

/// Signals a history refresh without moving plaintext through the event bus.
#[derive(Clone, serde::Serialize)]
pub(crate) struct IntegrationMessageAccepted {
    owner: String,
    account_id: String,
    destination: mesh_talk_integrations::Destination,
}
pub(crate) type AcceptanceCallback = Arc<dyn Fn(IntegrationMessageAccepted) + Send + Sync>;

fn denied(_: crate::commands::CommandError) -> IntegrationError {
    IntegrationError::new(
        "SCOPE_DENIED",
        "The desktop session is locked, retired, or replaced",
    )
}

struct DesktopHost {
    app: AppState,
    node: NodeState,
    lease: SessionLease,
    account_id: String,
    flush: Option<mpsc::Sender<FlushRequest>>,
    on_accept: Option<AcceptanceCallback>,
}
impl DesktopHost {
    fn new(app: AppState, node: NodeState, lease: SessionLease, account_id: String) -> Self {
        Self {
            app,
            node,
            lease,
            account_id,
            flush: None,
            on_accept: None,
        }
    }
    fn admission(&self) -> Result<RuntimeAdmission, IntegrationError> {
        RuntimeAdmission::capture_lease(&self.app, &self.node, &self.lease).map_err(denied)
    }
}
impl IntegrationHost for DesktopHost {
    fn account_id(&self) -> &str {
        &self.account_id
    }
    fn send<'a>(
        &'a self,
        request: OutgoingMessageRequest,
        authorization: Arc<dyn CommitAuthorization>,
    ) -> HostFuture<'a, MessageAcceptance> {
        Box::pin(async move {
            let admission = self.admission()?;
            // Runtime retirement must acquire exclusive access before taking the
            // profile, so a cancelled caller cannot leave a live producer behind.
            let guard = self.node.0.read().await;
            let runtime = guard.as_ref().ok_or_else(|| {
                IntegrationError::new("SCOPE_DENIED", "Desktop runtime is unavailable")
            })?;
            let acceptance = runtime
                .handle()
                .enqueue_message_once_if(&request, |operation| {
                    admission.authorize_accept(runtime, &mut || authorization.commit(operation))
                })
                .await?;
            if !acceptance.duplicate
                && let Some(on_accept) = &self.on_accept
            {
                let destination = match &request.destination {
                    MessageDestination::Account(id) => {
                        mesh_talk_integrations::Destination::Account { id: id.clone() }
                    }
                    MessageDestination::Channel(id) => {
                        mesh_talk_integrations::Destination::Channel {
                            id: hex::encode(id.as_bytes()),
                        }
                    }
                };
                // Losing event delivery or authority after acceptance must never
                // turn a durable send into an error inviting a duplicate retry.
                let _ = admission.checked(runtime, || {
                    on_accept(IntegrationMessageAccepted {
                        owner: self.lease.owner().to_owned(),
                        account_id: self.account_id.clone(),
                        destination,
                    });
                    Ok(())
                });
            }
            if let Some(flush) = &self.flush {
                // Acceptance is durable before notification. A full queue leaves
                // the record available to ordinary retry and synchronization.
                let _ = flush.try_send((request.destination, acceptance.message_id));
            }
            Ok(acceptance)
        })
    }
    fn with_node<'a>(&'a self, operation: HostOperation<'a>) -> HostFuture<'a, Value> {
        Box::pin(async move {
            self.admission()?
                .with_runtime(|runtime| Ok(operation(&runtime.handle())))
                .await
                .map_err(denied)?
        })
    }
}

struct AdminConfig {
    admin: PathBuf,
    manifest: PathBuf,
}
impl Drop for AdminConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.admin);
        let _ = std::fs::remove_file(&self.manifest);
    }
}

/// Called with exclusive access to the installed runtime. Both producers are owned by
/// its retirement barrier; no listener, flush, or encrypted grant writer escapes.
pub(crate) async fn start(
    app: &AppState,
    node: &NodeState,
    lease: &SessionLease,
    runtime: &mut NodeRuntime,
    base_dir: &Path,
    password: &str,
    on_accept: AcceptanceCallback,
) -> Result<(), IntegrationError> {
    let admission = RuntimeAdmission::capture_lease(app, node, lease).map_err(denied)?;
    let account_id = runtime.account_id().to_owned();
    let grants_path =
        crate::config_store::account_path(base_dir, lease.owner(), "integrations.grants")
            .ok_or_else(|| IntegrationError::new("INVALID_INPUT", "Invalid host profile"))?;
    let filename = format!(
        "integration-admin-{}-{}.json",
        lease.generation(),
        &new_token()[..16]
    );
    let admin_path = crate::config_store::account_path(base_dir, lease.owner(), &filename)
        .ok_or_else(|| IntegrationError::new("INVALID_INPUT", "Invalid host profile"))?;
    let manifest_path =
        crate::config_store::account_path(base_dir, lease.owner(), "integration-endpoint.json")
            .ok_or_else(|| IntegrationError::new("INVALID_INPUT", "Invalid host profile"))?;
    let endpoint_reference = endpoint_reference(&manifest_path)?;
    let listener = LocalServer::bind(&base_dir.join("ipc")).await?;
    let registry = admission
        .checked(runtime, || {
            Ok(GrantRegistry::open(&grants_path, password, &account_id))
        })
        .map_err(denied)??;
    let admin_token = new_token();
    admission
        .checked(runtime, || {
            Ok(write_private_json(
                &admin_path,
                &ClientConfig {
                    endpoint: endpoint_reference.clone(),
                    token: admin_token.clone(),
                },
            ))
        })
        .map_err(denied)??;
    let cleanup = AdminConfig {
        admin: admin_path,
        manifest: manifest_path.clone(),
    };
    // Producer files retain the manifest reference across host restarts while
    // random private socket addresses and the admin credential rotate each time.
    admission
        .checked(runtime, || {
            Ok(replace_private_json(
                &manifest_path,
                &EndpointManifest {
                    endpoint: listener.endpoint(),
                },
            ))
        })
        .map_err(denied)??;
    let (flush, mut pending) = mpsc::channel::<FlushRequest>(64);
    let mut host = DesktopHost::new(app.clone(), node.clone(), lease.clone(), account_id);
    host.flush = Some(flush);
    host.on_accept = Some(on_accept);
    let service = Arc::new(IntegrationService::new(
        Arc::new(host),
        registry,
        &admin_token,
    ));
    let participant = runtime.handle();
    runtime.register_host_task(tokio::spawn(async move {
        while let Some((destination, id)) = pending.recv().await {
            participant.flush_accepted_message(&destination, id).await;
        }
    }));
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
    runtime.register_graceful_host_task(
        shutdown,
        tokio::spawn(async move {
            let _cleanup = cleanup;
            if let Err(error) = listener
                .run_until(service, async {
                    let _ = shutdown_rx.await;
                })
                .await
            {
                log::warn!("Desktop integration listener stopped: {}", error.code);
            }
        }),
    );
    // The filename locates the private configuration without logging credentials
    // or persisting the host account identifier in diagnostics.
    log::info!("Desktop integrations enabled; account admin configuration: {filename}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_talk_integrations::service::IntegrationHost;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use tauri::Manager;

    fn fixture() -> (
        tempfile::TempDir,
        crate::state::AppState,
        crate::chat_commands::NodeState,
        crate::state::SessionLease,
    ) {
        let root = tempfile::tempdir().unwrap();
        let app = crate::state::AppState::new(crate::services::auth_service::AuthService::new(
            Arc::new(mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
            )),
        ));
        let user = crate::services::user::User {
            user_id: "integration-owner".into(),
            name: "integration".into(),
            display_name: "Integration".into(),
            address: "local".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        };
        let lease = app.session().publish("session".into(), user, "pw".into());
        (root, app, crate::chat_commands::NodeState::empty(), lease)
    }

    #[tokio::test]
    async fn desktop_integrations_emit_content_free_notice_once_after_acceptance() {
        struct Allow;
        impl CommitAuthorization for Allow {
            fn commit(
                &self,
                operation: &mut dyn FnMut() -> Result<(), mesh_talk_core::node::NodeError>,
            ) -> Result<(), mesh_talk_core::node::NodeError> {
                operation()
            }
        }
        let (root, app, node, lease) = fixture();
        let runtime = NodeRuntime::start(
            root.path(),
            lease.owner(),
            "Integration",
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
        let channel = runtime.create_channel("Alerts", vec![]).await.unwrap();
        let account_id = runtime.account_id().to_owned();
        *node.0.write().await = Some(runtime);
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = events.clone();
        let mut host = DesktopHost::new(app, node.clone(), lease.clone(), account_id.clone());
        host.on_accept = Some(Arc::new(move |event| {
            observed.lock().unwrap().push(event);
        }));
        let request = OutgoingMessageRequest {
            request_id: [7; 32],
            destination: MessageDestination::Channel(channel),
            text: b"private alert text".to_vec(),
            reply_to: None,
            expires_at_ms: None,
        };
        let first = host.send(request.clone(), Arc::new(Allow)).await.unwrap();
        assert!(!first.duplicate);
        assert!(host.send(request, Arc::new(Allow)).await.unwrap().duplicate);
        {
            let values = events.lock().unwrap();
            assert_eq!(values.len(), 1);
            assert_eq!(
                serde_json::to_value(&values[0]).unwrap(),
                serde_json::json!({
                    "owner":lease.owner(), "account_id":account_id,
                    "destination":{"kind":"channel","id":hex::encode(channel.as_bytes())}
                })
            );
        }
        let runtime = node.0.write().await.take().unwrap();
        runtime.stop().await;
    }

    #[tokio::test]
    async fn desktop_integrations_publish_stable_manifest_and_remove_it_on_retirement() {
        let (root, app, node, lease) = fixture();
        let runtime = NodeRuntime::start(
            root.path(),
            lease.owner(),
            "Integration",
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
        let mut guard = node.0.write().await;
        *guard = Some(runtime);
        start(
            &app,
            &node,
            &lease,
            guard.as_mut().unwrap(),
            root.path(),
            "pw",
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
        let profile = root.path().join("accounts").join(lease.owner());
        let manifest_path = profile.join("integration-endpoint.json");
        let manifest: mesh_talk_integrations::client::EndpointManifest =
            mesh_talk_integrations::private_files::read_private_json(&manifest_path).unwrap();
        assert!(!manifest.endpoint.is_empty());
        let admin_path = std::fs::read_dir(&profile)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("integration-admin-")
            })
            .unwrap();
        let admin: ClientConfig =
            mesh_talk_integrations::private_files::read_private_json(&admin_path).unwrap();
        assert_eq!(admin.endpoint, format!("file:{}", manifest_path.display()));
        drop(guard);
        let client = mesh_talk_integrations::client::LocalClient::from_file(&admin_path).unwrap();
        let context = client
            .call("admin_context", serde_json::json!({}))
            .await
            .unwrap();
        assert!(context["account_id"].is_string());
        let runtime = node.0.write().await.take().unwrap();
        runtime.stop().await;
        assert!(!admin_path.exists());
        assert!(!manifest_path.exists());
    }

    #[tokio::test]
    async fn desktop_integrations_reject_retired_runtime_before_node_callback() {
        let (_root, app, node, lease) = fixture();
        let host = DesktopHost::new(app, node, lease, "ab".repeat(16));
        let ran = Arc::new(AtomicBool::new(false));
        let observed = ran.clone();
        let result = host
            .with_node(Box::new(move |_| {
                observed.store(true, Ordering::SeqCst);
                Ok(serde_json::json!({}))
            }))
            .await;
        assert!(result.is_err());
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn desktop_integrations_recheck_lock_after_waiting_for_runtime() {
        let (root, app, node, lease) = fixture();
        let handle = tauri::test::mock_builder()
            .manage(app.clone())
            .manage(app.app_lock().clone())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let runtime = NodeRuntime::start(
            root.path(),
            lease.owner(),
            "Integration",
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
        let account = runtime.account_id().to_owned();
        let mut guard = node.0.write().await;
        *guard = Some(runtime);
        let (captured, received) = std::sync::mpsc::channel();
        *app.owner_command_captured.lock().unwrap() = Some(captured);
        let host = Arc::new(DesktopHost::new(app, node.clone(), lease, account));
        let ran = Arc::new(AtomicBool::new(false));
        let observed = ran.clone();
        let operation = tokio::spawn(async move {
            host.with_node(Box::new(move |_| {
                observed.store(true, Ordering::SeqCst);
                Ok(serde_json::json!({}))
            }))
            .await
        });
        // Entry authority is captured while this test retains the runtime mutex.
        // Lock the app before allowing the already-admitted request to proceed.
        tokio::task::spawn_blocking(move || {
            received.recv_timeout(std::time::Duration::from_secs(5))
        })
        .await
        .unwrap()
        .unwrap();
        crate::app_lock::lock_app(handle.state(), handle.state()).unwrap();
        drop(guard);
        assert!(operation.await.unwrap().is_err());
        assert!(!ran.load(Ordering::SeqCst));
        let runtime = node.0.write().await.take().unwrap();
        runtime.stop().await;
    }
}
