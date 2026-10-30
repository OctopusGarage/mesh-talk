//! Host authority for work against the installed participant runtime.
//!
//! Capture a session lease at command entry. Synchronous operations hold its
//! matching guard through the operation; asynchronous producers supply the
//! authorization callback at their actual core commit seam. Checking a lease
//! after a future completes does not prevent that future's side effects.
use crate::{
    chat_commands::NodeState,
    commands::CommandError,
    state::{AppState, SessionLease},
};
use mesh_talk_core::node::{NodeError, NodeRuntime};
use std::{future::Future, pin::Pin};

type AdmissionFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, CommandError>> + Send + 'a>>;

/// The owner captured at IPC entry, together with the only authority allowed to
/// run synchronous producers against the installed runtime. Detached operations
/// retain a shared lifecycle guard even if the caller cancels its IPC future.
/// A slow network producer must not exclude observations or unrelated sends;
/// replacing the runtime still waits for every admitted operation to finish.
#[derive(Clone)]
pub(crate) struct RuntimeAdmission {
    app: AppState,
    node: NodeState,
    lease: SessionLease,
    sensitive: Option<crate::app_lock::SensitiveAction>,
}

impl RuntimeAdmission {
    pub(crate) fn capture(
        app: &AppState,
        node: &NodeState,
        owner: &str,
    ) -> Result<Self, CommandError> {
        Self::capture_for(app, node, Some(owner))
    }

    /// Capture at command entry, before waiting for the installed runtime.
    /// Legacy payloads do not identify an earlier frontend session; this lease
    /// authorizes only the host session in which the command starts execution.
    pub(crate) fn capture_current(app: &AppState, node: &NodeState) -> Result<Self, CommandError> {
        Self::capture_for(app, node, None)
    }

    /// Reuse the exact startup session of a long-lived local integration host.
    /// Never substitute a newer session, even when the owner has not changed.
    pub(crate) fn capture_lease(
        app: &AppState,
        node: &NodeState,
        lease: &SessionLease,
    ) -> Result<Self, CommandError> {
        app.session()
            .matching(lease, |_| {
                if app.app_lock().is_locked() {
                    return Err(CommandError::Authorization("app locked".into()));
                }
                #[cfg(test)]
                if let Some(entered) = app.owner_command_captured.lock().unwrap().as_ref() {
                    let _ = entered.send("owner-entry");
                }
                Ok(Self {
                    app: app.clone(),
                    node: node.clone(),
                    lease: lease.clone(),
                    sensitive: None,
                })
            })
            .map_err(CommandError::Authorization)?
    }

    fn capture_for(
        app: &AppState,
        node: &NodeState,
        owner: Option<&str>,
    ) -> Result<Self, CommandError> {
        let lease = app
            .session()
            .capture()
            .map_err(CommandError::Authorization)?;
        if owner.is_some_and(|owner| lease.owner() != owner) {
            return Err(CommandError::Authorization("session owner mismatch".into()));
        }
        if app.app_lock().is_locked() {
            return Err(CommandError::Authorization("app locked".into()));
        }
        #[cfg(test)]
        if let Some(entered) = app.owner_command_captured.lock().unwrap().as_ref() {
            let _ = entered.send("owner-entry");
        }
        Ok(Self {
            app: app.clone(),
            node: node.clone(),
            lease,
            sensitive: None,
        })
    }

    pub(crate) async fn verify_identity(
        mut self,
        password: Option<String>,
        reason: &'static str,
    ) -> Result<Self, CommandError> {
        self.sensitive = Some(
            crate::app_lock::SensitiveAction::verify(&self.app, &self.lease, password, reason)
                .await?,
        );
        Ok(self)
    }

    pub(crate) fn sensitive_commit<T>(
        &self,
        operation: impl FnOnce() -> Result<T, CommandError>,
    ) -> Result<T, CommandError> {
        match &self.sensitive {
            Some(proof) => proof.commit(operation),
            None => operation(),
        }
    }

    pub(crate) fn lease(&self) -> &SessionLease {
        &self.lease
    }

    /// Atomically update private session metadata under the exact runtime installation.
    pub(crate) fn update_session<T>(
        &self,
        runtime: &NodeRuntime,
        operation: impl FnOnce(&mut crate::state::SessionInfo) -> Result<T, CommandError>,
    ) -> Result<T, CommandError> {
        self.app
            .session()
            .matching(&self.lease, |info| {
                self.node.check_installation(&self.lease)?;
                if runtime.host_account_id() != Some(self.lease.owner())
                    || self.app.app_lock().is_locked()
                {
                    return Err(CommandError::Authorization(
                        "runtime session unavailable".into(),
                    ));
                }
                self.sensitive_commit(|| operation(info))
            })
            .map_err(CommandError::Authorization)?
    }

    /// Enclose a synchronous operation in current installation and session authority.
    pub(crate) fn checked<T>(
        &self,
        runtime: &NodeRuntime,
        operation: impl FnOnce() -> Result<T, CommandError>,
    ) -> Result<T, CommandError> {
        self.node
            .with_session_runtime(self.app.session(), &self.lease, runtime, || {
                if self.app.app_lock().is_locked() {
                    return Err(CommandError::Authorization("app locked".into()));
                }
                match &self.sensitive {
                    Some(proof) => proof.commit(operation),
                    None => operation(),
                }
            })
    }

    /// Run a synchronous observation or mutation while both runtime and session
    /// guards and one app-lock revision guard are held. A verified sensitive
    /// action already owns that last guard; never acquire it a second time.
    pub(crate) fn checked_at_lock_revision<T>(
        &self,
        runtime: &NodeRuntime,
        revision: u64,
        operation: impl FnOnce() -> Result<T, CommandError>,
    ) -> Result<T, CommandError> {
        self.node
            .with_session_runtime(self.app.session(), &self.lease, runtime, || {
                match &self.sensitive {
                    Some(proof) => proof.commit_at_revision(revision, operation),
                    None => self
                        .app
                        .app_lock()
                        .with_unchanged_revision(revision, operation),
                }
            })
    }

    /// Run a synchronous observation or mutation while both runtime and session
    /// guards are held. A returned Node handle does not carry this authority
    /// into later asynchronous work; producers must authorize at their commit.
    pub(crate) async fn with_runtime<T>(
        &self,
        operation: impl FnOnce(&NodeRuntime) -> Result<T, CommandError>,
    ) -> Result<T, CommandError> {
        let guard = tokio::time::timeout(std::time::Duration::from_secs(5), self.node.0.read())
            .await
            .map_err(|_| CommandError::Internal("runtime admission timed out".into()))?;
        let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
        self.checked(runtime, || operation(runtime))
    }

    pub(crate) fn authorize_accept(
        &self,
        runtime: &NodeRuntime,
        accept: &mut dyn FnMut() -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        // The session guard encloses each synchronous producer (local grant and WAL),
        // not just a preceding check. No session guard crosses an async policy wait.
        let result = self
            .checked(runtime, || Ok(accept()))
            .map_err(|error| NodeError::Authorization(error.to_string()))?;
        #[cfg(test)]
        if result.is_ok()
            && let Some(entered) = self.app.owner_command_captured.lock().unwrap().as_ref()
        {
            let _ = entered.send("owner-authorized");
        }
        result
    }

    /// Enclose a synchronous privacy commit with the same session, installation
    /// and app-lock authority as durable acceptance. Core calls this outside
    /// its state lock after awaiting the policy gate.
    pub(crate) fn authorize_io(
        &self,
        runtime: &NodeRuntime,
        commit: &mut dyn FnMut() -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        self.checked(runtime, || Ok(commit())).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, error.to_string())
        })?
    }

    /// Blocking producers retain runtime lifecycle ownership through completion,
    /// including cancellation of their originating IPC future.
    pub(crate) async fn blocking<T: Send + 'static>(
        self,
        operation: impl FnOnce(&NodeRuntime, &Self) -> Result<T, CommandError> + Send + 'static,
    ) -> Result<T, CommandError> {
        let guard = self.node.0.clone().read_owned().await;
        tokio::task::spawn_blocking(move || {
            let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
            self.checked(runtime, || operation(runtime, &self))
        })
        .await
        .map_err(|_| CommandError::Internal("blocking runtime operation terminated".into()))?
    }

    pub(crate) async fn detached<T: Send + 'static>(
        self,
        operation: impl for<'a> FnOnce(&'a NodeRuntime, &'a Self) -> AdmissionFuture<'a, T>
        + Send
        + 'static,
    ) -> Result<T, CommandError> {
        let guard = self.node.0.clone().read_owned().await;
        tokio::spawn(async move {
            let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
            #[cfg(test)]
            if let Some(entered) = self.app.owner_command_captured.lock().unwrap().as_ref() {
                // Distinguish a live detached owner from queued mutex admission.
                let _ = entered.send("owner-producer");
            }
            self.checked(runtime, || Ok(()))?;
            operation(runtime, &self).await
        })
        .await
        .map_err(|_| CommandError::Internal("enqueue operation terminated".into()))?
    }
}

#[cfg(test)]
mod integration_lease_tests {
    use super::*;

    fn fixture() -> (
        tempfile::TempDir,
        AppState,
        NodeState,
        crate::services::user::User,
    ) {
        let root = tempfile::tempdir().unwrap();
        let app = AppState::new(crate::services::auth_service::AuthService::new(
            std::sync::Arc::new(mesh_talk_core::identity::manager::IdentityManager::new(
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
        (root, app, NodeState::empty(), user)
    }

    #[tokio::test]
    async fn stalled_producer_allows_observation_but_prevents_runtime_retirement() {
        let (root, app, node, user) = fixture();
        app.session().publish("token".into(), user, "pw".into());
        let runtime = NodeRuntime::start(
            root.path(),
            "integration-owner",
            "Owner",
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
        let channel = runtime
            .handle()
            .create_channel("Concurrent send", vec![])
            .await
            .unwrap();
        *node.0.write().await = Some(runtime);
        let producer = RuntimeAdmission::capture_current(&app, &node).unwrap();
        let observer = RuntimeAdmission::capture_current(&app, &node).unwrap();
        let sender = RuntimeAdmission::capture_current(&app, &node).unwrap();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, blocked) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(producer.detached(move |_, _| {
            Box::pin(async move {
                entered.send(()).unwrap();
                blocked.await.unwrap();
                Ok(())
            })
        }));
        started.await.unwrap();
        let observed = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            observer.with_runtime(|runtime| Ok(runtime.user_id().to_owned())),
        )
        .await;
        let sent = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            sender.detached(move |runtime, admission| {
                Box::pin(async move {
                    runtime
                        .handle()
                        .send_channel_message_reply_expiring_if(
                            channel,
                            b"hi",
                            None,
                            None,
                            |commit| admission.authorize_accept(runtime, commit),
                        )
                        .await
                        .map_err(CommandError::from)
                })
            }),
        )
        .await;
        let retirement_blocked = node.0.try_write().is_err();
        release.send(()).unwrap();
        task.await.unwrap().unwrap();
        node.0.write().await.take().unwrap().stop().await;
        assert!(
            observed.is_ok_and(|result| result.is_ok()),
            "a stalled send must not block identity/history/presence observations"
        );
        assert!(
            retirement_blocked,
            "retirement must still wait for the active producer"
        );
        assert!(
            sent.is_ok_and(|result| result.is_ok()),
            "a stalled producer must not queue an unrelated channel send"
        );
    }

    #[test]
    fn integrations_lease_rejects_replacement_login_even_for_same_owner() {
        let (_root, app, node, user) = fixture();
        let original = app
            .session()
            .publish("old".into(), user.clone(), "pw".into());
        assert!(RuntimeAdmission::capture_lease(&app, &node, &original).is_ok());
        app.session().publish("new".into(), user, "pw".into());
        assert!(RuntimeAdmission::capture_lease(&app, &node, &original).is_err());
    }

    #[test]
    fn integrations_lease_rejects_logout() {
        let (_root, app, node, user) = fixture();
        let lease = app.session().publish("old".into(), user, "pw".into());
        app.session().clear();
        assert!(RuntimeAdmission::capture_lease(&app, &node, &lease).is_err());
    }
}
