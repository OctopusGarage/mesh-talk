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
const FILE_REPAIR_INTERVAL_SECS: u64 = 300;

// Scheduling evidence only. Actual file admission keeps current consent and
// transport authority checks; ordinary lease renewals do not represent new work.
pub(super) struct FilePullPeer {
    public: crate::identity::device::PublicIdentity,
    account: Option<String>,
    post_office: bool,
    preferred: std::net::SocketAddr,
    routes: Vec<(std::net::SocketAddr, crate::discovery::roster::RouteSource)>,
    fresh_until: Option<std::time::Instant>,
}
impl FilePullPeer {
    fn changes_work(&self, next: &Self, now: std::time::Instant) -> bool {
        self.public != next.public
            || self.account != next.account
            || self.post_office != next.post_office
            || self.preferred != next.preferred
            || self.routes != next.routes
            || (self.fresh_until.is_none_or(|at| at <= now)
                && next.fresh_until.is_some_and(|at| at > now))
    }
}

impl Node {
    pub(super) fn file_pull_peers(&self) -> Vec<FilePullPeer> {
        let now = std::time::Instant::now();
        let peers = self.roster.lock().expect("roster lock").peers();
        let mut snapshot: Vec<_> = peers
            .into_iter()
            .map(|peer| {
                let fresh_until = peer.presence_deadline(now);
                let mut routes: Vec<_> = peer
                    .routes
                    .iter()
                    .map(|route| (route.addr, route.source))
                    .collect();
                routes.sort_unstable_by_key(|route| (route.0, route.1 as u8));
                FilePullPeer {
                    public: peer.public,
                    account: peer.account_id,
                    post_office: peer.post_office,
                    preferred: peer.addr,
                    routes,
                    fresh_until,
                }
            })
            .collect();
        snapshot.sort_unstable_by_key(|peer| peer.public.user_id());
        snapshot
    }

    pub(super) async fn wait_for_file_pull(
        &self,
        network: &mut tokio::sync::watch::Receiver<u64>,
        roster: &mut tokio::sync::watch::Receiver<u64>,
        available: &mut tokio::sync::watch::Receiver<u64>,
        channels: &mut tokio::sync::broadcast::Receiver<crate::eventlog::ConversationId>,
        peers: &mut Vec<FilePullPeer>,
        wait: Duration,
    ) -> bool {
        self.wait_for_attachment_work(
            &self.file_pull_notify,
            network,
            roster,
            available,
            channels,
            peers,
            wait,
        )
        .await
    }

    // Both lanes observe the same meaningful route/policy events but consume
    // independent retained notifications; neither owns the other's retry clock.
    #[allow(clippy::too_many_arguments)]
    async fn wait_for_attachment_work(
        &self,
        notify: &Notify,
        network: &mut tokio::sync::watch::Receiver<u64>,
        roster: &mut tokio::sync::watch::Receiver<u64>,
        available: &mut tokio::sync::watch::Receiver<u64>,
        channels: &mut tokio::sync::broadcast::Receiver<crate::eventlog::ConversationId>,
        peers: &mut Vec<FilePullPeer>,
        wait: Duration,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + wait;
        let mut channels_open = true;
        loop {
            tokio::select! {
                _ = notify.notified() => return true,
                _ = network.changed() => return true,
                _ = available.changed() => return true,
                event = channels.recv(), if channels_open => {
                    match event {
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return true,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => channels_open = false,
                    }
                },
                _ = roster.changed() => {
                    let next = self.file_pull_peers();
                    let now = std::time::Instant::now();
                    let changed = peers.len() != next.len()
                        || peers.iter().zip(&next).any(|(old, new)| old.changes_work(new, now));
                    // Refresh the captured lease even when ignoring a heartbeat.
                    // A later expired-to-fresh transition at the same endpoint
                    // must wake work without treating every renewal as demand.
                    *peers = next;
                    if changed { return true; }
                },
                _ = tokio::time::sleep_until(deadline) => return false,
            }
        }
    }
    pub(super) fn group_file_schedule(&self) -> (Duration, bool) {
        let pending = self.group_file_work_pending();
        let wait = Duration::from_secs(if pending {
            FILE_PULL_INTERVAL_SECS
        } else {
            FILE_REPAIR_INTERVAL_SECS
        });
        (
            self.group_file_retry_delay()
                .map_or(wait, |retry| wait.min(retry)),
            pending,
        )
    }

    /// Observe retained work after a pull before scheduling this producer's wait.
    pub(super) fn file_pull_schedule(&self, delay: Duration) -> (Duration, bool) {
        let (downloading, preparing) = {
            let files = self.files.lock().expect("files lock");
            let downloading = files.download_work_pending().unwrap_or_else(|error| {
                log::warn!("download scheduling observation failed: {error}");
                true
            });
            let preparing = files
                .download_preparation_pending()
                .unwrap_or_else(|error| {
                    log::warn!("download preparation observation failed: {error}");
                    false
                });
            (downloading, preparing)
        };
        let local = preparing
            || self.pending_channel_file().is_some()
            || self.media_maintenance_pending().unwrap_or_else(|error| {
                log::warn!("media scheduling observation failed: {error}");
                false
            });
        let pending = downloading || local;
        let wait = if local {
            delay.min(Duration::from_secs(FILE_PULL_INTERVAL_SECS))
        } else if pending {
            delay.clamp(
                Duration::from_secs(FILE_PULL_INTERVAL_SECS),
                Duration::from_secs(FILE_REPAIR_INTERVAL_SECS),
            )
        } else {
            delay.max(Duration::from_secs(FILE_REPAIR_INTERVAL_SECS))
        };
        (wait, pending)
    }
}

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
        let discovery = spawn_discovery_with_network_changes(
            Arc::clone(&socket),
            Arc::clone(&roster),
            Arc::clone(&announce),
            self_uid.clone(),
            discovery_port,
            Some(Arc::clone(&discovery_trigger)),
            Arc::clone(&visibility),
            Some(Arc::clone(&routed)),
            Some(node.network_changes.clone()),
        );
        #[cfg(test)]
        let ambient_discovery_ids = discovery.ambient_ids();
        #[cfg(test)]
        let ambient_discovery_first_polls = discovery.ambient_first_polls();
        tasks.extend(discovery.into_handles());
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
                            } {
                                let _ = visibility
                                    .with_routed_send(routed.probe(sender, endpoint))
                                    .await;
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
                let mut available = node.subscribe_sibling_availability();
                let mut channels = node.subscribe_channel_changes();
                let mut peers = node.file_pull_peers();
                let mut delay = Duration::from_secs(FILE_PULL_INTERVAL_SECS);
                loop {
                    let Some(permit) = node.runtime_work.admit() else { break; };
                    let receiving = Arc::clone(&node);
                    // The blocking owner destroys captured Node references
                    // before releasing retirement admission, including when
                    // the awaiting producer is cancelled.
                    if let Err(error) = permit.spawn_blocking(move || {
                        receiving.retry_file_receives();
                        if let Err(error)=receiving.maintain_media(){log::warn!("media maintenance deferred: {error}");}
                    }).await {
                        log::warn!("file receive worker failed: {error}");
                    }
                    tokio::select! {
                        _ = node.pull_pending_files_direct() => {},
                        _ = network.changed() => { delay = Duration::from_secs(FILE_PULL_INTERVAL_SECS); continue; },
                    }
                    let (wait, _pending) = node.file_pull_schedule(delay);
                    let changed = node.wait_for_file_pull(&mut network, &mut roster, &mut available, &mut channels, &mut peers, wait).await;
                    delay = if changed {
                        Duration::from_secs(FILE_PULL_INTERVAL_SECS)
                    } else {
                        (delay * 2).min(Duration::from_secs(FILE_REPAIR_INTERVAL_SECS))
                    };
                }
            }));
        }

        {
            let node = Arc::clone(&node);
            tasks.push(tokio::spawn(async move {
                let mut network = node.network_changes.subscribe();
                let mut roster = node.subscribe_roster_changes();
                let mut available = node.subscribe_sibling_availability();
                let mut channels = node.subscribe_channel_changes();
                let mut files = node.subscribe_file_changes();
                let mut peers = node.file_pull_peers();
                loop {
                    if node.runtime_work.is_closed() {
                        break;
                    }
                    tokio::select! {
                        _ = node.retry_group_file_delivery() => {},
                        _ = network.changed() => continue,
                    }
                    let (wait, _) = node.group_file_schedule();
                    tokio::select! {
                        _ = node.wait_for_attachment_work(
                            &node.group_file_notify, &mut network, &mut roster,
                            &mut available, &mut channels, &mut peers, wait,
                        ) => {},
                        _ = files.changed() => {},
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
            #[cfg(test)]
            ambient_discovery_ids,
            #[cfg(test)]
            ambient_discovery_first_polls,
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
    #[cfg(test)]
    ambient_discovery_ids: [tokio::task::Id; 3],
    #[cfg(test)]
    ambient_discovery_first_polls: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
#[derive(Debug)]
pub(super) struct AmbientDiscoveryRetirement {
    pub first_polls: usize,
    pub removed: usize,
    pub joined: usize,
    pub failures: Vec<tokio::task::JoinError>,
}

#[cfg(test)]
async fn retire_named_ambient_discovery(
    tasks: &mut Vec<JoinHandle<()>>,
    health: &mut Vec<tokio::task::AbortHandle>,
    ids: &[tokio::task::Id; 3],
    first_polls: &std::sync::atomic::AtomicUsize,
) -> AmbientDiscoveryRetirement {
    let mut ambient = Vec::with_capacity(3);
    let mut retained = Vec::with_capacity(tasks.len());
    for task in tasks.drain(..) {
        if ids.contains(&task.id()) {
            ambient.push(task);
        } else {
            retained.push(task);
        }
    }
    *tasks = retained;
    for task in &ambient {
        task.abort();
    }
    health.retain(|task| !ambient.iter().any(|removed| removed.id() == task.id()));
    let mut report = AmbientDiscoveryRetirement {
        first_polls: first_polls.load(std::sync::atomic::Ordering::SeqCst),
        removed: ambient.len(),
        joined: 0,
        failures: Vec::new(),
    };
    // Every removed owner is awaited even if another producer panicked or an
    // expected task was absent. The caller checks the report after cleanup.
    for task in ambient {
        match task.await {
            Ok(()) => report.joined += 1,
            Err(error) if error.is_cancelled() => report.joined += 1,
            Err(error) => report.failures.push(error),
        }
    }
    report
}

#[cfg(test)]
mod ambient_retirement_tests {
    use super::*;

    #[tokio::test]
    async fn missing_named_task_and_panicked_producer_preserve_failure_and_join_other_owners() {
        let (entered, ready) = tokio::sync::oneshot::channel();
        let panicked = tokio::spawn(async move {
            let _ = entered.send(());
            panic!("owned ambient panic control");
        });
        let entered = ready.await;
        let cancelled = tokio::spawn(std::future::pending::<()>());
        let missing = tokio::spawn(std::future::pending::<()>());
        let unrelated = tokio::spawn(std::future::pending::<()>());
        let ids = [panicked.id(), cancelled.id(), missing.id()];
        let survivor_ids = [missing.id(), unrelated.id()];
        let mut health = vec![
            panicked.abort_handle(),
            cancelled.abort_handle(),
            missing.abort_handle(),
            unrelated.abort_handle(),
        ];
        let mut tasks = vec![panicked, cancelled, unrelated];
        let mut report = retire_named_ambient_discovery(
            &mut tasks,
            &mut health,
            &ids,
            &std::sync::atomic::AtomicUsize::new(0),
        )
        .await;
        let retained_ids: Vec<_> = tasks.iter().map(JoinHandle::id).collect();
        let health_ids: std::collections::HashSet<_> =
            health.iter().map(tokio::task::AbortHandle::id).collect();
        let unrelated_live = tasks.iter().all(|task| !task.is_finished());
        let missing_live = !missing.is_finished();
        for task in &tasks {
            task.abort();
        }
        missing.abort();
        let mut cleanup = Vec::new();
        for task in tasks {
            cleanup.push(task.await);
        }
        cleanup.push(missing.await);
        assert_eq!(report.removed, 2);
        assert_eq!(report.joined, 1);
        assert_eq!(report.failures.len(), 1);
        let failure = report.failures.pop().unwrap();
        assert!(failure.is_panic());
        let payload = failure.into_panic();
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str));
        assert_eq!(message, Some("owned ambient panic control"));
        assert!(entered.is_ok());
        assert_eq!(retained_ids, vec![survivor_ids[1]]);
        assert_eq!(health_ids, survivor_ids.into_iter().collect());
        assert!(unrelated_live && missing_live);
        assert!(
            cleanup
                .into_iter()
                .all(|result| result.is_err_and(|error| error.is_cancelled()))
        );
    }
}

struct GracefulHostTask {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl ParticipantRuntime {
    #[cfg(test)]
    pub(super) fn discovery_trace(&self) -> &RoutedDiscovery {
        &self.routed_discovery
    }

    #[cfg(test)]
    pub(super) fn ambient_discovery_first_polls(&self) -> usize {
        self.ambient_discovery_first_polls
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Explicit loopback fixtures retain the listener and all TCP/delivery work.
    /// Abort every named ambient producer before the first join can yield.
    #[cfg(test)]
    pub(super) async fn retire_ambient_discovery_for_test(&mut self) -> AmbientDiscoveryRetirement {
        retire_named_ambient_discovery(
            &mut self.tasks,
            &mut self.discovery_health,
            &self.ambient_discovery_ids,
            &self.ambient_discovery_first_polls,
        )
        .await
    }

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
        let socket = match endpoint {
            std::net::SocketAddr::V4(_) => &self.discovery_socket_v4,
            std::net::SocketAddr::V6(_) => self.discovery_socket_v6.as_ref().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "IPv6 unavailable")
            })?,
        };
        let sent = self
            .visibility
            .with_routed_send(self.routed_discovery.probe_if(socket, endpoint, authorize))
            .await?
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "routed discovery is disabled",
                )
            })?;
        if sent {
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
        let prepared_retirement = self.node.retire_prepared_sync_views();
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
        if let Some(retirement) = prepared_retirement {
            let _ = retirement.await;
        }
        self.node.runtime_work.drain().await;
    }
}

impl Drop for ParticipantRuntime {
    fn drop(&mut self) {
        // Drop requests retirement; its admitted blocking owner keeps the actual
        // profile leases until cleanup ends even without an awaited shutdown.
        drop(self.node.retire_prepared_sync_views());
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
