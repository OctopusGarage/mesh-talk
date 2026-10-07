use crate::commands::CommandError;
use mesh_talk_core::node::{Node, NodeRuntime};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Managed state holding the current session's node runtime (`None` until login).
#[derive(Clone)]
pub struct NodeState(
    pub Arc<Mutex<Option<NodeRuntime>>>,
    pub(crate) Arc<Mutex<()>>,
    pub(crate) Arc<std::sync::atomic::AtomicU64>,
    pub(crate) Arc<std::sync::Mutex<Option<(crate::state::SessionLease, u64)>>>,
    #[cfg(test)] pub(crate) Arc<std::sync::Mutex<Option<(std::path::PathBuf, u16)>>>,
);

impl NodeState {
    /// Called only under the runtime lifecycle and matching Session guards.
    pub(crate) fn check_installation(
        &self,
        lease: &crate::state::SessionLease,
    ) -> Result<(), CommandError> {
        use std::sync::atomic::Ordering;
        let installed = self
            .3
            .lock()
            .map_err(|_| CommandError::Internal("runtime state unavailable".into()))?;
        if let Some((current, ticket)) = installed.as_ref() {
            if current != lease || *ticket != self.2.load(Ordering::Acquire) {
                return Err(CommandError::Authorization(
                    "runtime session replaced".into(),
                ));
            }
        } else if self.2.load(Ordering::Acquire) != 0 {
            return Err(CommandError::not_started());
        }
        Ok(())
    }

    pub fn empty() -> Self {
        NodeState(
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(())),
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
            Arc::new(std::sync::Mutex::new(None)),
            #[cfg(test)]
            Arc::new(std::sync::Mutex::new(None)),
        )
    }

    /// Clone the node before a command starts async or blocking work. The session
    /// lock is released when this method returns, so other commands can proceed.
    pub(crate) async fn node_handle(&self) -> Result<Arc<Node>, CommandError> {
        let guard = self.0.lock().await;
        let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
        Ok(runtime.handle())
    }
}

impl Default for NodeState {
    fn default() -> Self {
        Self::empty()
    }
}
