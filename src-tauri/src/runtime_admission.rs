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
/// retain the lifecycle guard even if the caller cancels its IPC future.
pub(crate) struct RuntimeAdmission {
    app: AppState,
    node: NodeState,
    lease: SessionLease,
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
        })
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
                operation()
            })
    }

    /// Run a synchronous observation or mutation while both runtime and session
    /// guards are held. A returned Node handle does not carry this authority
    /// into later asynchronous work; producers must authorize at their commit.
    pub(crate) async fn with_runtime<T>(
        &self,
        operation: impl FnOnce(&NodeRuntime) -> Result<T, CommandError>,
    ) -> Result<T, CommandError> {
        let guard = self.node.0.lock().await;
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

    pub(crate) async fn detached<T: Send + 'static>(
        self,
        operation: impl for<'a> FnOnce(&'a NodeRuntime, &'a Self) -> AdmissionFuture<'a, T>
        + Send
        + 'static,
    ) -> Result<T, CommandError> {
        let guard = self.node.0.clone().lock_owned().await;
        tokio::spawn(async move {
            let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
            #[cfg(test)]
            if let Some(entered) = self.app.owner_command_captured.lock().unwrap().as_ref() {
                // Distinguish a live detached owner from queued mutex admission.
                let _ = entered.send("owner-producer");
            }
            operation(runtime, &self).await
        })
        .await
        .map_err(|_| CommandError::Internal("enqueue operation terminated".into()))?
    }
}
