use crate::commands::CommandError;
use mesh_talk_core::node::{Node, NodeRuntime};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Managed state holding the current session's node runtime (`None` until login).
#[derive(Clone)]
pub struct NodeState(pub Arc<Mutex<Option<NodeRuntime>>>);

impl NodeState {
    pub fn empty() -> Self {
        NodeState(Arc::new(Mutex::new(None)))
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
