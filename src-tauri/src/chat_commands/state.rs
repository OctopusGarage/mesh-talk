use crate::commands::CommandError;
use crate::state::{SessionInfo, SessionLease, SessionState};
use mesh_talk_core::node::{Node, NodeRuntime};
use std::future::Future;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Managed state holding the current session's node runtime (`None` until login).
#[derive(Clone)]
pub struct NodeState(
    pub Arc<Mutex<Option<NodeRuntime>>>,
    Arc<Mutex<()>>,
    Arc<std::sync::atomic::AtomicU64>,
    Arc<std::sync::Mutex<Option<(crate::state::SessionLease, u64)>>>,
    #[cfg(test)] pub(crate) Arc<std::sync::Mutex<Option<(std::path::PathBuf, u16)>>>,
);

impl NodeState {
    /// Serialize offline recovery with runtime startup and retirement.
    pub(crate) async fn lock_profile_lifecycle(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.1.clone().lock_owned().await
    }

    #[cfg(test)]
    pub(crate) fn test_startup_gate(&self) -> &Arc<Mutex<()>> {
        &self.1
    }

    #[cfg(test)]
    pub(crate) fn test_lifecycle_ticket(&self) -> u64 {
        self.2.load(std::sync::atomic::Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn test_installed_for(&self, lease: &SessionLease) -> bool {
        self.3
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(current, _)| current == lease)
    }

    /// Retire only the runtime owned by this generation. The caller may be
    /// cancelled after publishing a new session; the spawned retirement still
    /// holds startup serialization until the old runtime has stopped.
    pub(crate) async fn retire_if_current(
        &self,
        ticket: u64,
        before_stop: impl Future<Output = ()>,
    ) {
        let _startup = self.1.clone().lock_owned().await;
        if self.2.load(std::sync::atomic::Ordering::Acquire) != ticket {
            return;
        }
        let old = self.0.lock().await.take();
        if let Some(old) = old {
            before_stop.await;
            old.stop().await;
        }
    }

    pub(crate) fn next_lifecycle_ticket(&self) -> u64 {
        self.2.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1
    }

    fn ticket_current(&self, ticket: u64) -> bool {
        self.2.load(std::sync::atomic::Ordering::Acquire) == ticket
    }

    fn record_installation(&self, lease: SessionLease, ticket: u64) {
        *self.3.lock().unwrap() = Some((lease, ticket));
    }

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

    /// Legacy runtime snapshot, not session authority. The runtime lock is
    /// released before return; new synchronous commands use RuntimeAdmission,
    /// and asynchronous producers authorize at their actual commit seam.
    pub(crate) async fn node_handle(&self) -> Result<Arc<Node>, CommandError> {
        let guard = self.0.lock().await;
        let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
        Ok(runtime.handle())
    }

    /// Test adapter for exercising the shared installation/session checks.
    /// Production operations use RuntimeAdmission to retain their entry lease.
    #[cfg(test)]
    pub(crate) async fn node_for_owner(
        &self,
        session: &SessionState,
        owner: &str,
    ) -> Option<Arc<Node>> {
        let lease = session.capture().ok()?;
        if lease.owner() != owner {
            return None;
        }
        let guard = self.0.lock().await;
        let runtime = guard.as_ref()?;
        self.with_session_runtime(session, &lease, runtime, || Ok(runtime.handle()))
            .ok()
    }

    /// Called under the runtime guard. Keep the matching session guard through
    /// synchronous work so session publication cannot split check from commit.
    pub(crate) fn with_session_runtime<T>(
        &self,
        session: &SessionState,
        lease: &SessionLease,
        runtime: &NodeRuntime,
        operation: impl FnOnce() -> Result<T, CommandError>,
    ) -> Result<T, CommandError> {
        session
            .matching(lease, |_| {
                if runtime.host_account_id() != Some(lease.owner()) {
                    return Err(CommandError::Authorization("runtime owner mismatch".into()));
                }
                self.check_installation(lease)?;
                operation()
            })
            .map_err(CommandError::Authorization)?
    }
}

impl Default for NodeState {
    fn default() -> Self {
        Self::empty()
    }
}

#[derive(Clone)]
pub(crate) struct RuntimeAuthority {
    session: SessionState,
    pub(crate) lease: SessionLease,
    node: crate::chat_commands::NodeState,
    pub(crate) ticket: u64,
}

impl RuntimeAuthority {
    pub(crate) fn request(
        session: SessionState,
        lease: SessionLease,
        node: crate::chat_commands::NodeState,
    ) -> Result<Self, String> {
        let ticket = session.matching(&lease, |_| node.next_lifecycle_ticket())?;
        Ok(Self {
            session,
            lease,
            node,
            ticket,
        })
    }

    pub(crate) async fn begin(&self) -> Result<StartupPermit, String> {
        let serialization = self.node.1.clone().lock_owned().await;
        self.current(|_| ())?;
        let old = self.node.0.lock().await.take();
        if let Some(old) = old {
            old.stop().await;
        }
        let display_name = self.current(|info| info.user.display_name.clone())?;
        Ok(StartupPermit {
            authority: self.clone(),
            _serialization: serialization,
            display_name,
        })
    }

    pub(crate) fn current<T>(
        &self,
        operation: impl FnOnce(&mut SessionInfo) -> T,
    ) -> Result<T, String> {
        self.session.matching(&self.lease, |info| {
            if !self.node.ticket_current(self.ticket) {
                return Err("startup superseded".into());
            }
            Ok(operation(info))
        })?
    }
}

pub(crate) struct StartupPermit {
    authority: RuntimeAuthority,
    _serialization: tokio::sync::OwnedMutexGuard<()>,
    pub(crate) display_name: String,
}

impl StartupPermit {
    pub(crate) async fn install(self, runtime: NodeRuntime) -> bool {
        let mut guard = self.authority.node.0.lock().await;
        let mut runtime = Some(runtime);
        let installed = self
            .authority
            .current(|info| {
                let rt = runtime.as_mut().unwrap();
                if rt.display_name() != info.user.display_name {
                    rt.set_display_name(&info.user.display_name);
                }
                *guard = runtime.take();
                self.authority
                    .node
                    .record_installation(self.authority.lease.clone(), self.authority.ticket);
            })
            .is_ok();
        drop(guard);
        if !installed {
            runtime.unwrap().stop().await;
        }
        installed
    }
}
