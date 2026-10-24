//! Session-bound desktop host for the local integrations service.

pub(crate) mod instructions;
pub(crate) mod local_access;

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
    http::LocalHttpServer,
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
use tokio::sync::{mpsc, watch};

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
    enabled: Arc<std::sync::Mutex<bool>>,
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
            enabled: Arc::new(std::sync::Mutex::new(true)),
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
                    admission.authorize_accept(runtime, &mut || {
                        let enabled = self.enabled.lock().map_err(|_| {
                            mesh_talk_core::node::NodeError::Authorization(
                                "Integration state unavailable".into(),
                            )
                        })?;
                        if !*enabled {
                            return Err(mesh_talk_core::node::NodeError::Authorization(
                                "Integrations disabled".into(),
                            ));
                        }
                        authorization.commit(operation)
                    })
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
                .with_runtime(|runtime| {
                    let enabled = self.enabled.lock().map_err(|_| {
                        crate::commands::CommandError::Authorization(
                            "Integration state unavailable".into(),
                        )
                    })?;
                    if !*enabled {
                        return Err(crate::commands::CommandError::Authorization(
                            "Integrations disabled".into(),
                        ));
                    }
                    Ok(operation(&runtime.handle()))
                })
                .await
                .map_err(denied)?
        })
    }
}

/// Control of the existing desktop listener, not another integration service.
/// The runtime retains ownership of every producer and its retirement barrier.
pub(crate) struct DesktopIntegration {
    // The runtime owns the service; retaining it here would cycle through DesktopHost.app.
    service: std::sync::Weak<IntegrationService>,
    admin_token: String,
    pub(crate) http_endpoint: String,
    client_files: Arc<std::sync::Mutex<Vec<PathBuf>>>,
    enabled: Arc<std::sync::Mutex<bool>>,
    shutdown: watch::Sender<bool>,
    stopped: watch::Receiver<bool>,
}
impl DesktopIntegration {
    pub(crate) fn track_client_file(&self, path: PathBuf) {
        self.client_files.lock().unwrap().push(path);
    }
    pub(crate) fn is_running(&self) -> bool {
        !*self.stopped.borrow() && self.enabled.lock().is_ok_and(|enabled| *enabled)
    }
    pub(crate) fn close_admission(&self) -> Result<(), IntegrationError> {
        *self.enabled.lock().map_err(|_| {
            IntegrationError::new("STORAGE_FAILURE", "Integration state unavailable")
        })? = false;
        Ok(())
    }
    pub(crate) async fn admin(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, IntegrationError> {
        let service = self.service.upgrade().ok_or_else(|| {
            IntegrationError::new("SCOPE_DENIED", "Integration service is no longer running")
        })?;
        service.handle(&self.admin_token, method, params).await
    }
    pub(crate) async fn stop(&mut self) -> Result<(), IntegrationError> {
        self.close_admission()?;
        self.shutdown.send_replace(true);
        let _ = self.stopped.wait_for(|stopped| *stopped).await;
        Ok(())
    }
}

struct AdminConfig {
    admin: PathBuf,
    manifest: PathBuf,
    enabled: Arc<std::sync::Mutex<bool>>,
    stopped: watch::Sender<bool>,
    client_files: Arc<std::sync::Mutex<Vec<PathBuf>>>,
}
impl Drop for AdminConfig {
    fn drop(&mut self) {
        if let Ok(mut enabled) = self.enabled.lock() {
            *enabled = false;
        }
        let _ = std::fs::remove_file(&self.admin);
        let _ = std::fs::remove_file(&self.manifest);
        if let Ok(paths) = self.client_files.lock() {
            for path in paths.iter() {
                let _ = std::fs::remove_file(path);
            }
        }
        self.stopped.send_replace(true);
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
) -> Result<DesktopIntegration, IntegrationError> {
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
    let mut registry = admission
        .checked(runtime, || {
            Ok(GrantRegistry::open(&grants_path, password, &account_id))
        })
        .map_err(denied)??;
    // Managed desktop credentials never carry authority into another listener
    // generation. CLI/MCP grants retain their existing restart behavior.
    admission
        .checked(runtime, || {
            Ok((|| {
                for grant in registry
                    .list()?
                    .into_iter()
                    .filter(|grant| grant.client_id == local_access::CLIENT_ID)
                {
                    registry.revoke(&grant.id)?;
                }
                Ok::<_, IntegrationError>(())
            })())
        })
        .map_err(denied)??;
    let http = LocalHttpServer::bind()
        .await?
        .with_client_config(base_dir.join("agent-access.json"));
    let http_endpoint = http.endpoint();
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
    let enabled = Arc::new(std::sync::Mutex::new(true));
    let (stopped, stopped_rx) = watch::channel(false);
    let client_files = Arc::new(std::sync::Mutex::new(Vec::new()));
    let cleanup = AdminConfig {
        admin: admin_path,
        manifest: manifest_path.clone(),
        enabled: enabled.clone(),
        stopped,
        client_files: client_files.clone(),
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
    host.enabled = enabled.clone();
    host.flush = Some(flush);
    host.on_accept = Some(on_accept);
    let service = Arc::new(IntegrationService::new(
        Arc::new(host),
        registry,
        &admin_token,
    ));
    let participant = runtime.handle();
    let (stop, mut stop_rx) = watch::channel(false);
    let handle = DesktopIntegration {
        service: Arc::downgrade(&service),
        admin_token,
        http_endpoint,
        client_files,
        enabled: enabled.clone(),
        shutdown: stop.clone(),
        stopped: stopped_rx,
    };
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
    // One runtime-owned supervisor owns both transport adapters and the flush
    // producer. JoinSet also aborts its children if runtime Drop cancels it.
    runtime.register_graceful_host_task(
        shutdown,
        tokio::spawn(async move {
            let _cleanup = cleanup;
            let mut workers = tokio::task::JoinSet::new();
            let flush = workers.spawn(async move {
                while let Some((destination, id)) = pending.recv().await {
                    participant.flush_accepted_message(&destination, id).await;
                }
            });
            let mut uds_stop = stop.subscribe();
            let uds_service = service.clone();
            workers.spawn(async move {
                let _ = listener
                    .run_until(uds_service, async move {
                        let _ = uds_stop.wait_for(|stop| *stop).await;
                    })
                    .await;
            });
            let mut http_stop = stop.subscribe();
            workers.spawn(async move {
                let _ = http
                    .run_until(service, async move {
                        let _ = http_stop.wait_for(|stop| *stop).await;
                    })
                    .await;
            });
            tokio::select! {
                _ = shutdown_rx => {},
                _ = stop_rx.wait_for(|stop| *stop) => {},
                _ = workers.join_next() => {},
            }
            if let Ok(mut enabled) = enabled.lock() {
                *enabled = false;
            }
            stop.send_replace(true);
            flush.abort();
            let drained = tokio::time::timeout(std::time::Duration::from_secs(32), async {
                while workers.join_next().await.is_some() {}
            })
            .await;
            if drained.is_err() {
                workers.abort_all();
                while workers.join_next().await.is_some() {}
            }
        }),
    );
    // The filename locates the private configuration without logging credentials
    // or persisting the host account identifier in diagnostics.
    log::info!("Desktop integrations enabled; account admin configuration: {filename}");
    Ok(handle)
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
            account_login_name: None,
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
    async fn desktop_control_handle_does_not_retain_retired_service() {
        let (root, app, node, lease) = fixture();
        let mut runtime = NodeRuntime::start(
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
        let integration = start(
            &app,
            &node,
            &lease,
            &mut runtime,
            root.path(),
            "pw",
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
        let service = integration.service.clone();
        *node.0.write().await = Some(runtime);
        node.0.write().await.take().unwrap().stop().await;
        assert!(
            service.upgrade().is_none(),
            "a retired service must not be retained by its settings control handle"
        );
    }

    #[tokio::test]
    async fn desktop_integrations_can_stop_without_retiring_the_node() {
        let (root, app, node, lease) = fixture();
        let mut runtime = NodeRuntime::start(
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
        let mut integration = start(
            &app,
            &node,
            &lease,
            &mut runtime,
            root.path(),
            "pw",
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
        assert!(integration.is_running());
        *node.0.write().await = Some(runtime);
        assert!(
            integration
                .admin("admin_context", serde_json::json!({}))
                .await
                .is_ok()
        );
        integration.close_admission().unwrap();
        assert!(
            integration
                .admin("admin_context", serde_json::json!({}))
                .await
                .is_err()
        );
        integration.stop().await.unwrap();
        assert!(!integration.is_running());
        assert!(
            node.0
                .read()
                .await
                .as_ref()
                .unwrap()
                .create_channel("Still running", vec![])
                .await
                .is_ok()
        );
        drop(integration);
        node.0.write().await.take().unwrap().stop().await;
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
