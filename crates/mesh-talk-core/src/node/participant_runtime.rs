//! Owned participant producers shared by desktop and headless hosts.
//! Preparation binds resources without spawning work; launch is synchronous so
//! hosts can perform their final session authorization around it.
use super::{Node, runtime_work::RuntimeWork};
use crate::discovery::service::{
    RoutedDiscovery, SharedAnnounce, shared_announce, spawn_discovery_with_network_changes,
    spawn_routed_listener, swap_announce,
};
use crate::discovery::{DiscoveryVisibility, candidates::CandidateSource};
use crate::transport::net::{discovery_socket, routed_ipv6_socket};
use std::{sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, UdpSocket},
    sync::Notify,
    task::{JoinHandle, JoinSet},
};

const FILE_PULL_INTERVAL_SECS: u64 = 3;

/// Network inventory supplied by the host; identity and roster come from Node.
pub struct ParticipantConfig {
    pub discovery_port: u16,
    pub candidate_source: Option<Arc<dyn CandidateSource>>,
}

/// Bound resources with no running producers. Dropping releases the listener.
pub struct PreparedParticipant {
    node: Arc<Node>,
    listener: TcpListener,
    visibility: Arc<DiscoveryVisibility>,
    socket: Arc<UdpSocket>,
    socket_v6: Option<Arc<UdpSocket>>,
    routed: Arc<RoutedDiscovery>,
    config: ParticipantConfig,
    tcp_port: u16,
}

impl PreparedParticipant {
    pub fn prepare(
        node: Arc<Node>,
        listener: TcpListener,
        visibility: Arc<DiscoveryVisibility>,
        config: ParticipantConfig,
    ) -> std::io::Result<Self> {
        let tcp_port = listener.local_addr()?.port();
        let socket = Arc::new(discovery_socket(config.discovery_port)?);
        let socket_v6 = routed_ipv6_socket(config.discovery_port).ok().map(Arc::new);
        let routed = Arc::new(RoutedDiscovery::new({
            let node = Arc::clone(&node);
            Arc::new(move |nonce, announce| node.signed_discovery_response(nonce, announce))
        }));
        Ok(Self {
            node,
            listener,
            visibility,
            socket,
            socket_v6,
            routed,
            config,
            tcp_port,
        })
    }

    /// Launch only after the host has initialized Node privacy and authorized use.
    pub fn launch(self, display_name: &str) -> ParticipantRuntime {
        let Self {
            node,
            listener,
            visibility,
            socket,
            socket_v6,
            routed,
            config,
            tcp_port,
        } = self;
        let discovery_port = config.discovery_port;
        let candidate_source = config.candidate_source;
        let roster = Arc::clone(&node.roster);
        let self_uid = node.identity.public().user_id();
        let current_announce = node.signed_announce(display_name, tcp_port);
        let _ = node.update_presence(&current_announce);
        let announce = shared_announce(&current_announce);
        let discovery_trigger = Arc::new(Notify::new());
        let mut tasks: Vec<JoinHandle<()>> = Vec::new();
        tasks.extend(spawn_discovery_with_network_changes(
            Arc::clone(&socket),
            Arc::clone(&roster),
            Arc::clone(&announce),
            self_uid.clone(),
            discovery_port,
            Some(Arc::clone(&discovery_trigger)),
            Arc::clone(&visibility),
            Some(Arc::clone(&routed)),
            Some(node.network_changes.clone()),
        ));
        if let Some(socket_v6) = socket_v6.as_ref() {
            tasks.push(spawn_routed_listener(
                Arc::clone(socket_v6),
                Arc::clone(&roster),
                Arc::clone(&announce),
                self_uid.clone(),
                discovery_port,
                Arc::clone(&visibility),
                Arc::clone(&routed),
            ));
        }
        let discovery_health = tasks.iter().map(JoinHandle::abort_handle).collect();
        if let Some(source) = candidate_source {
            let socket = Arc::clone(&socket);
            let socket_v6 = socket_v6.clone();
            let visibility = Arc::clone(&visibility);
            let routed = Arc::clone(&routed);
            let trigger = Arc::clone(&discovery_trigger);
            tasks.push(tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(5));
                loop {
                    tokio::select! {
                        _ = tick.tick() => {},
                        _ = trigger.notified() => {},
                    }
                    if visibility.routed_allowed().await
                        && let Ok(candidates) =
                            tokio::time::timeout(Duration::from_secs(3), source.snapshot()).await
                    {
                        let mut unique = std::collections::HashSet::new();
                        for endpoint in candidates
                            .into_iter()
                            .filter(|addr| unique.insert(*addr))
                            .take(256)
                        {
                            if !visibility.routed_allowed().await {
                                break;
                            }
                            if let Some(sender) = match endpoint {
                                std::net::SocketAddr::V4(_) => Some(&socket),
                                std::net::SocketAddr::V6(_) => socket_v6.as_ref(),
                            } && let Some(_guard) = visibility.routed_send_guard().await
                            {
                                let _ = routed.probe(sender, endpoint).await;
                            }
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                    }
                }
            }));
        }
        tasks.push(tokio::spawn(Arc::clone(&node).run_accept_loop(listener)));
        {
            let node = Arc::clone(&node);
            tasks.push(tokio::spawn(node.run_private_route_probes()));
        }
        {
            let node = Arc::clone(&node);
            tasks.push(tokio::spawn(node.run_post_office_drains()));
        }
        {
            let node = Arc::clone(&node);
            tasks.push(tokio::spawn(node.run_bridge_directory()));
        }
        {
            let node = Arc::clone(&node);
            tasks.push(tokio::spawn(async move {
                let mut network = node.network_changes.subscribe();
                let mut roster = node.roster.lock().expect("roster lock not poisoned").subscribe_changes();
                let mut delay = Duration::from_secs(FILE_PULL_INTERVAL_SECS);
                loop {
                    let Some(permit) = node.runtime_work.admit() else { break; };
                    let receiving = Arc::clone(&node);
                    if let Err(error) = tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        receiving.retry_file_receives();
                    }).await {
                        log::warn!("file receive worker failed: {error}");
                    }
                    let pending = node.files.lock().expect("files lock").download_work_pending().unwrap_or_else(|error| {log::warn!("download scheduling observation failed: {error}");true}) || node.group_file_work_pending();
                    tokio::select! {
                        _ = node.pull_pending_files() => {},
                        _ = network.changed() => { delay = Duration::from_secs(FILE_PULL_INTERVAL_SECS); continue; },
                    }
                    tokio::select! {
                        _ = node.file_pull_notify.notified() => { delay = Duration::from_secs(FILE_PULL_INTERVAL_SECS); },
                        _ = network.changed() => { delay = Duration::from_secs(FILE_PULL_INTERVAL_SECS); },
                        _ = roster.changed() => { delay = Duration::from_secs(FILE_PULL_INTERVAL_SECS); },
                        _ = tokio::time::sleep(delay) => { delay = if pending { Duration::from_secs(FILE_PULL_INTERVAL_SECS) } else { (delay * 2).min(Duration::from_secs(60)) }; },
                    }
                }
            }));
        }

        ParticipantRuntime {
            node,
            announce,
            tcp_port,
            discovery_port,
            discovery_socket_v4: socket,
            discovery_socket_v6: socket_v6,
            routed_discovery: routed,
            visibility,
            discovery_trigger,
            discovery_health,
            tasks,
            graceful_host_tasks: Vec::new(),
        }
    }
}

/// Owns network producers and host tasks for one initialized Node.
/// Call stop before reopening its stores; Drop only requests cancellation.
pub struct ParticipantRuntime {
    node: Arc<Node>,
    announce: SharedAnnounce,
    tcp_port: u16,
    discovery_port: u16,
    discovery_socket_v4: Arc<UdpSocket>,
    discovery_socket_v6: Option<Arc<UdpSocket>>,
    routed_discovery: Arc<RoutedDiscovery>,
    pub(super) visibility: Arc<DiscoveryVisibility>,
    discovery_trigger: Arc<Notify>,
    discovery_health: Vec<tokio::task::AbortHandle>,
    pub(super) tasks: Vec<JoinHandle<()>>,
    graceful_host_tasks: Vec<GracefulHostTask>,
}

struct GracefulHostTask {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl ParticipantRuntime {
    /// Local discovery producer health; makes no claim about Internet or LAN reachability.
    pub fn discovery_available(&self) -> bool {
        self.discovery_health.iter().all(|task| !task.is_finished())
    }

    /// Adopt a host producer so retirement cancels and joins it too.
    pub fn register_host_task(&mut self, task: JoinHandle<()>) {
        self.tasks.retain(|task| !task.is_finished());
        self.tasks.push(task);
    }

    /// Register a producer that owns child tasks or other async cleanup. On
    /// retirement it receives shutdown and must join its children before exiting.
    /// Unlike an ordinary host task, stop waits for this cleanup without aborting it.
    pub fn register_graceful_host_task(
        &mut self,
        shutdown: tokio::sync::oneshot::Sender<()>,
        task: JoinHandle<()>,
    ) {
        self.graceful_host_tasks
            .retain(|owner| !owner.task.is_finished());
        self.graceful_host_tasks.push(GracefulHostTask {
            shutdown: Some(shutdown),
            task,
        });
    }

    pub fn blocking_work(&self) -> ParticipantWork {
        ParticipantWork(Arc::clone(&self.node.runtime_work))
    }

    pub fn listen_tcp_port(&self) -> u16 {
        self.tcp_port
    }
    pub fn discovery_port(&self) -> u16 {
        self.discovery_port
    }

    /// Update the signed presence and wake discovery, respecting Node privacy.
    pub fn set_display_name(&self, display_name: &str) -> std::io::Result<()> {
        let announce = self.node.signed_announce(display_name, self.tcp_port);
        self.node.update_presence(&announce)?;
        swap_announce(&self.announce, &announce);
        self.trigger_discovery();
        Ok(())
    }

    pub fn trigger_discovery(&self) {
        self.discovery_trigger.notify_waiters();
    }

    /// Probe with the same privacy gate and IPv4/IPv6 socket policy as candidates.
    pub async fn probe_endpoint(&self, endpoint: std::net::SocketAddr) -> std::io::Result<()> {
        self.probe_endpoint_if(endpoint, |commit| commit()).await
    }

    pub async fn probe_endpoint_if(
        &self,
        endpoint: std::net::SocketAddr,
        authorize: impl FnMut(&mut dyn FnMut() -> std::io::Result<()>) -> std::io::Result<()> + Send,
    ) -> std::io::Result<()> {
        if !self.visibility.routed_allowed().await {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "public discovery is disabled",
            ));
        }
        let socket = match endpoint {
            std::net::SocketAddr::V4(_) => &self.discovery_socket_v4,
            std::net::SocketAddr::V6(_) => self.discovery_socket_v6.as_ref().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "IPv6 unavailable")
            })?,
        };
        let _guard = self.visibility.routed_send_guard().await.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "routed discovery is disabled",
            )
        })?;
        if self
            .routed_discovery
            .probe_if(socket, endpoint, authorize)
            .await?
        {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "too many pending discovery queries",
            ))
        }
    }

    /// Close admission, cancel and join producers, then wait for admitted
    /// blocking work. Externally held Node handles remain the host's responsibility.
    pub async fn stop(mut self) {
        self.node.runtime_work.close();
        self.node.clear_delivery_observations();
        for owner in &mut self.graceful_host_tasks {
            if let Some(shutdown) = owner.shutdown.take() {
                let _ = shutdown.send(());
            }
        }
        for task in &self.tasks {
            task.abort();
        }
        for task in std::mem::take(&mut self.tasks) {
            let _ = task.await;
        }
        for owner in std::mem::take(&mut self.graceful_host_tasks) {
            let _ = owner.task.await;
        }
        self.node.runtime_work.drain().await;
    }
}

impl Drop for ParticipantRuntime {
    fn drop(&mut self) {
        self.node.runtime_work.close();
        self.node.clear_delivery_observations();
        for owner in &mut self.graceful_host_tasks {
            if let Some(shutdown) = owner.shutdown.take() {
                let _ = shutdown.send(());
            }
            owner.task.abort();
        }
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Blocking work admitted to the participant's retirement barrier. A permit is
/// held inside the closure even if its JoinSet or awaiting caller is cancelled.
#[derive(Clone)]
pub struct ParticipantWork(Arc<RuntimeWork>);
impl ParticipantWork {
    pub fn spawn<T: Send + 'static>(
        &self,
        tasks: &mut JoinSet<T>,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> bool {
        let Some(permit) = self.0.admit() else {
            return false;
        };
        tasks.spawn_blocking(move || {
            let _permit = permit;
            work()
        });
        true
    }
}
