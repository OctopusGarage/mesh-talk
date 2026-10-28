use crate::commands::CommandError;
use crate::state::{SessionInfo, SessionLease, SessionState};
use mesh_talk_core::node::{Node, NodeRuntime};
use std::future::Future;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// Managed runtime slot (`None` until login). Operations borrow it concurrently;
/// installation and retirement require exclusive access to the slot.
#[derive(Clone)]
pub struct NodeState(
    pub Arc<RwLock<Option<NodeRuntime>>>,
    Arc<Mutex<()>>,
    Arc<std::sync::atomic::AtomicU64>,
    Arc<std::sync::Mutex<LifecycleState>>,
    #[cfg(test)] pub(crate) Arc<std::sync::Mutex<Option<(std::path::PathBuf, u16)>>>,
);

#[derive(Default)]
struct LifecycleState {
    installed: Option<(SessionLease, u64)>,
    startup: Option<StartupObservation>,
}

struct StartupObservation {
    lease: SessionLease,
    ticket: u64,
    projection: StartupIdentity,
}

/// IDs-only presentation observation; it never admits operations against a node.
#[derive(Clone, serde::Serialize)]
pub struct StartupIdentity {
    owner: String,
    startup_id: String,
    phase: StartupPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_id: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum StartupPhase {
    Pending,
    Identity,
    Ready,
    Failed,
}

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
            .installed
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
        let old = self.0.write().await.take();
        if let Some(old) = old {
            before_stop.await;
            old.stop().await;
        }
    }

    pub(crate) fn next_lifecycle_ticket(&self) -> u64 {
        let mut state = self.3.lock().unwrap();
        state.startup = None;
        self.2.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1
    }

    fn ticket_current(&self, ticket: u64) -> bool {
        self.2.load(std::sync::atomic::Ordering::Acquire) == ticket
    }

    fn record_installation(&self, lease: SessionLease, ticket: u64) {
        let mut state = self.3.lock().unwrap();
        state.installed = Some((lease.clone(), ticket));
        if let Some(startup) = state.startup.as_mut()
            && startup.lease == lease
            && startup.ticket == ticket
        {
            startup.projection.phase = StartupPhase::Ready;
        }
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
        if let Some((current, ticket)) = installed.installed.as_ref() {
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
            Arc::new(RwLock::new(None)),
            Arc::new(Mutex::new(())),
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
            Arc::new(std::sync::Mutex::new(LifecycleState::default())),
            #[cfg(test)]
            Arc::new(std::sync::Mutex::new(None)),
        )
    }

    pub(crate) fn startup_identity(
        &self,
        session: &SessionState,
        lease: &SessionLease,
        app_lock: &crate::app_lock::AppLock,
    ) -> Result<Option<StartupIdentity>, CommandError> {
        let revision = app_lock.authorization_revision()?;
        session
            .matching(lease, |_| {
                app_lock.with_unchanged_revision(revision, || {
                    let state = self
                        .3
                        .lock()
                        .map_err(|_| CommandError::Internal("runtime state unavailable".into()))?;
                    Ok(state
                        .startup
                        .as_ref()
                        .filter(|startup| {
                            startup.lease == *lease && self.ticket_current(startup.ticket)
                        })
                        .map(|startup| startup.projection.clone()))
                })
            })
            .map_err(CommandError::Authorization)?
    }

    /// Legacy runtime snapshot, not session authority. The runtime lock is
    /// released before return; new synchronous commands use RuntimeAdmission,
    /// and asynchronous producers authorize at their actual commit seam.
    pub(crate) async fn node_handle(&self) -> Result<Arc<Node>, CommandError> {
        let guard = tokio::time::timeout(std::time::Duration::from_secs(5), self.0.read())
            .await
            .map_err(|_| CommandError::Internal("runtime observation timed out".into()))?;
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
        let guard = self.0.read().await;
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
        let ticket = session.matching(&lease, |_| {
            use rand::distr::{Alphanumeric, Distribution};
            let ticket = node.next_lifecycle_ticket();
            node.3.lock().unwrap().startup = Some(StartupObservation {
                lease: lease.clone(),
                ticket,
                projection: StartupIdentity {
                    owner: lease.owner().to_owned(),
                    startup_id: Alphanumeric
                        .sample_iter(rand::rng())
                        .take(32)
                        .map(char::from)
                        .collect(),
                    phase: StartupPhase::Pending,
                    account_id: None,
                    device_id: None,
                },
            });
            ticket
        })?;
        Ok(Self {
            session,
            lease,
            node,
            ticket,
        })
    }

    pub(crate) fn observe_identity(
        &self,
        identity: mesh_talk_core::node::RuntimeIdentity,
    ) -> Result<(), String> {
        self.current(|_| {
            let mut state = self.node.3.lock().unwrap();
            let startup = state.startup.as_mut().ok_or("startup unavailable")?;
            if startup.lease != self.lease || startup.ticket != self.ticket {
                return Err("startup superseded");
            }
            startup.projection.phase = StartupPhase::Identity;
            startup.projection.account_id = Some(identity.account_id);
            startup.projection.device_id = Some(identity.device_id);
            Ok(())
        })?
        .map_err(str::to_owned)
    }

    pub(crate) fn observation_lifetime(&self) -> StartupObservationLifetime {
        StartupObservationLifetime(self.clone())
    }

    pub(crate) async fn begin(&self) -> Result<StartupPermit, String> {
        let serialization = self.node.1.clone().lock_owned().await;
        self.current(|_| ())?;
        let old = self.node.0.write().await.take();
        let retired = match old {
            Some(old) => Some(old.stop_for_storage().await),
            None => None,
        };
        let display_name = self.current(|info| info.user.display_name.clone())?;
        Ok(StartupPermit {
            authority: self.clone(),
            _serialization: std::sync::Arc::new(serialization),
            retired,
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

/// Failure and cancellation retire only this startup's observation. A successful
/// installation keeps its public identity until the next lifecycle request.
pub(crate) struct StartupObservationLifetime(RuntimeAuthority);

impl Drop for StartupObservationLifetime {
    fn drop(&mut self) {
        let _ = self.0.current(|_| {
            let mut state = self.0.node.3.lock().unwrap();
            if let Some(startup) = state.startup.as_mut()
                && startup.lease == self.0.lease
                && startup.ticket == self.0.ticket
                && startup.projection.phase != StartupPhase::Ready
            {
                startup.projection.phase = StartupPhase::Failed;
                startup.projection.account_id = None;
                startup.projection.device_id = None;
            }
        });
    }
}

pub(crate) struct StartupPermit {
    authority: RuntimeAuthority,
    retired: Option<mesh_talk_core::node::RetiredNode>,
    pub(crate) display_name: String,
    // Retained profile handles must drop before replacement admission opens.
    _serialization: std::sync::Arc<tokio::sync::OwnedMutexGuard<()>>,
}

impl StartupPermit {
    pub(crate) fn take_retired(&mut self) -> Option<mesh_talk_core::node::RetiredNode> {
        self.retired.take()
    }

    pub(crate) fn track_startup(&self, completion: mesh_talk_core::node::StartupCompletion) {
        let serialization = self._serialization.clone();
        tokio::spawn(async move {
            completion.wait().await;
            drop(serialization);
        });
    }
    pub(crate) async fn install(self, runtime: NodeRuntime) -> bool {
        let mut guard = self.authority.node.0.write().await;
        let mut runtime = Some(runtime);
        let installed = self
            .authority
            .current(|info| {
                let rt = runtime.as_mut().unwrap();
                if info.user.display_name != self.display_name
                    && rt.display_name() != info.user.display_name
                {
                    // An explicit rename committed while startup prepared must
                    // also advance the shared account claim before installation.
                    if let Err(error) = rt
                        .handle()
                        .set_account_display_name(&info.user.display_name)
                    {
                        log::warn!("Unable to publish startup nickname: {error}");
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn runtime_observation_deadline_releases_the_waiter_and_allows_recovery() {
        let node = NodeState::empty();
        let held = node.0.write().await;
        let result = tokio::time::timeout(std::time::Duration::from_secs(6), node.node_handle())
            .await
            .unwrap();
        assert!(
            matches!(result, Err(CommandError::Internal(message)) if message.contains("timed out"))
        );
        drop(held);
        assert!(matches!(
            node.node_handle().await,
            Err(CommandError::NotStarted(_))
        ));
    }
}
