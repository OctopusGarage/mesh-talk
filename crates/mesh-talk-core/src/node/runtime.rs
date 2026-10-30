//! The desktop App context for the node: opens a per-user `Node`, starts
//! its network loops (discovery, accept, drain) and an inbound-DM forwarder, and
//! holds the handles. Tauri-agnostic — inbound DMs are delivered via an injected
//! `on_dm` callback, so this is unit-testable without a GUI.

use super::{ParticipantConfig, ParticipantRuntime, PreparedParticipant};
use crate::discovery::candidates::CandidateSource;
use crate::discovery::{PeerRecord, Roster};
use crate::eventlog::{ConversationId, LogError};
use crate::identity::account_keystore;
use crate::identity::device::PublicIdentity;
use crate::identity::keystore;
use crate::node::name_directory::NameDirectory;
use crate::node::{HistoryEntry, Node, NodeError, ReceivedDm};
#[cfg(test)]
use crate::transport::net::routed_ipv6_socket;
use futures_util::{StreamExt, future::BoxFuture, stream::FuturesUnordered};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// How often the node checks for new peers and retries pending public profiles.
const PROFILE_REPUBLISH_INTERVAL_SECS: u64 = 3;
/// Keep background profile traffic within the shared connection budget.
const MAX_PROFILE_PUBLICATIONS: usize = 2;

#[cfg(test)]
type InitializationHook = Box<dyn FnOnce() + Send>;
#[cfg(test)]
static NAMES_OPEN_HOOKS: std::sync::LazyLock<
    Mutex<std::collections::HashMap<std::path::PathBuf, InitializationHook>>,
> = std::sync::LazyLock::new(Default::default);

#[cfg(test)]
static PRIVACY_OPEN_HOOKS: std::sync::LazyLock<
    Mutex<std::collections::HashMap<std::path::PathBuf, InitializationHook>>,
> = std::sync::LazyLock::new(Default::default);

struct ProfilePublication {
    revision: u64,
    conversation: ConversationId,
    complete: bool,
    last_attempt: Option<std::time::Instant>,
}

/// Keep publication state only for currently present peers. Missing or
/// outdated revisions remain eligible until a direct or relay exchange succeeds.
fn profile_republish_targets(
    published: &mut std::collections::HashMap<String, ProfilePublication>,
    peers: &[PeerRecord],
    revision: u64,
) -> Vec<PeerRecord> {
    let mut present_now = std::collections::HashSet::new();
    let mut targets = Vec::new();
    for p in peers {
        if p.post_office {
            continue;
        }
        let uid = p.public.user_id();
        if !published
            .get(&uid)
            .is_some_and(|publication| publication.revision == revision && publication.complete)
        {
            targets.push(p.clone());
        }
        present_now.insert(uid);
    }
    published.retain(|uid, _| present_now.contains(uid));
    // Untouched peers precede retries, then the oldest attempt goes first.
    targets.sort_by_key(|peer| {
        published
            .get(&peer.public.user_id())
            .and_then(|publication| publication.last_attempt)
    });
    targets
}

fn channel_invitee_identity(peer: PeerRecord) -> Option<PublicIdentity> {
    (!peer.post_office).then_some(peer.public)
}

/// Errors starting the node runtime.
#[derive(Debug)]
pub enum RuntimeError {
    /// Opening the keystore or the durable logs failed.
    Open(LogError),
    /// Binding the TCP listener or discovery socket failed.
    Io(std::io::Error),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::Open(e) => write!(f, "node open error: {e}"),
            RuntimeError::Io(e) => write!(f, "node io error: {e}"),
        }
    }
}

impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Open(error) => Some(error),
            Self::Io(error) => Some(error),
        }
    }
}

impl From<LogError> for RuntimeError {
    fn from(e: LogError) -> Self {
        RuntimeError::Open(e)
    }
}
impl From<std::io::Error> for RuntimeError {
    fn from(e: std::io::Error) -> Self {
        RuntimeError::Io(e)
    }
}

/// Host settings for one running node. `account_id` namespaces local data;
/// the cryptographic account ID comes from the account keystore.
pub struct RuntimeConfig<'a> {
    pub base_dir: &'a Path,
    pub account_id: &'a str,
    pub display_name: &'a str,
    pub password: &'a str,
    pub discovery_port: u16,
    pub post_office_enabled: bool,
    /// Optional host-owned routed address inventory; the core never starts a VPN.
    pub candidate_source: Option<Arc<dyn CandidateSource>>,
}

/// Additive startup policy options; legacy RuntimeConfig callers retain defaults.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeNetworkOptions {
    pub post_office_send_enabled: bool,
    pub post_office_receive_enabled: bool,
    pub physical_lan_only: bool,
}
impl Default for RuntimeNetworkOptions {
    fn default() -> Self {
        Self {
            post_office_send_enabled: true,
            post_office_receive_enabled: true,
            physical_lan_only: false,
        }
    }
}

/// Public identifiers authenticated by the startup keystore unlocks.
/// This observation does not grant access to a prepared or running node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeIdentity {
    pub device_id: String,
    pub account_id: String,
}

/// Inbound events forwarded to the host from independent runtime tasks.
pub struct RuntimeEvents {
    pub on_dm: Box<dyn Fn(ReceivedDm) + Send + 'static>,
    pub on_channel: Box<dyn Fn(crate::node::channel::ReceivedChannelMessage) + Send + 'static>,
    pub on_file: Box<dyn Fn(crate::node::filebook::ReceivedFile) + Send + 'static>,
    pub on_profile: Box<dyn Fn(crate::node::ReceivedProfile) + Send + 'static>,
    pub on_call_signal: Box<dyn Fn(crate::node::ReceivedCallSignal) + Send + 'static>,
}

/// A running node plus the background tasks that drive it. Use [`Self::stop`]
/// before reopening its stores. Drop only requests best-effort cancellation.
pub struct NodeRuntime {
    node: Arc<Node>,
    roster: Arc<Mutex<Roster>>,
    /// Durable directory of last-seen peer display names. Outlives the live roster (which
    /// evicts a peer ~30s after it stops announcing), so an offline member keeps its name.
    names: Arc<Mutex<NameDirectory>>,
    user_id: String,
    account_id: String,
    /// Exact host local-user namespace, independent of cryptographic account ID.
    host_owner: String,
    /// The display name this node advertises (for the diagnostics page).
    display_name: Mutex<String>,
    participant: ParticipantRuntime,
    /// Where the account keystore lives, so a successful device link can persist the
    /// adopted account secret (effective on next login).
    account_path: std::path::PathBuf,
    /// The session password, to re-encrypt the keystore when adopting a linked account.
    password: String,
}

/// Joined runtime producers with the original storage owner retained for a
/// host-serialized offline export. External SDK calls remain host responsibility.
pub struct RetiredNode {
    node: Arc<Node>,
    host_owner: String,
}
impl RetiredNode {
    /// Originating host local-user label; never a cryptographic account ID.
    pub fn host_account_id(&self) -> &str {
        &self.host_owner
    }
    /// Physical locator used by the serialized host to match a retired handoff.
    pub fn profile_root(&self) -> &Path {
        self.node.storage.root()
    }
    pub fn node(&self) -> &Node {
        &self.node
    }
    pub fn close(self) -> Result<(), LogError> {
        self.node.close_profile()
    }
}

impl NodeRuntime {
    /// Include host producers in this profile's shutdown and join boundary.
    pub fn register_host_task(&mut self, task: tokio::task::JoinHandle<()>) {
        self.participant.register_host_task(task);
    }

    /// Keep a host's asynchronous child cleanup inside profile retirement.
    pub fn register_graceful_host_task(
        &mut self,
        shutdown: tokio::sync::oneshot::Sender<()>,
        task: tokio::task::JoinHandle<()>,
    ) {
        self.participant.register_graceful_host_task(shutdown, task);
    }

    pub fn blocking_work(&self) -> super::participant_runtime::ParticipantWork {
        self.participant.blocking_work()
    }

    /// Retire runtime producers before a host reopens this profile. Admission
    /// closes first, then task futures are cancelled and joined, and admitted
    /// blocking operations finish. No timeout permits a live writer to escape.
    ///
    /// This does not cancel arbitrary SDK calls through externally held Node
    /// references; hosts must also serialize their own profile operations.
    pub async fn stop(self) {
        drop(self.stop_for_storage().await);
    }

    pub async fn stop_for_storage(self) -> RetiredNode {
        self.participant.stop().await;
        RetiredNode {
            node: self.node,
            host_owner: self.host_owner,
        }
    }
    /// Compatibility constructor with positional startup arguments.
    /// New hosts should use [`Self::start_configured`] or
    /// [`Self::start_configured_guarded`].
    ///
    /// `account_id` is the host app's account identifier — it only namespaces the
    /// data directory so each logged-in user gets their own keystore/logs. It is
    /// distinct from the node's own cryptographic fingerprint ([`Self::user_id`]),
    /// which is derived from the keystore identity and is what peers see.
    pub async fn start(
        base_dir: &Path,
        account_id: &str,
        display_name: &str,
        password: &str,
        discovery_port: u16,
        on_dm: impl Fn(ReceivedDm) + Send + 'static,
        on_channel: impl Fn(crate::node::channel::ReceivedChannelMessage) + Send + 'static,
        on_file: impl Fn(crate::node::filebook::ReceivedFile) + Send + 'static,
        on_profile: impl Fn(crate::node::ReceivedProfile) + Send + 'static,
        on_call_signal: impl Fn(crate::node::ReceivedCallSignal) + Send + 'static,
    ) -> Result<NodeRuntime, RuntimeError> {
        Self::start_guarded(
            base_dir,
            account_id,
            display_name,
            password,
            discovery_port,
            on_dm,
            on_channel,
            on_file,
            on_profile,
            on_call_signal,
            |launch| launch(display_name),
        )
        .await
    }

    /// Start a node using named host settings and inbound event handlers.
    pub async fn start_configured(
        config: RuntimeConfig<'_>,
        events: RuntimeEvents,
    ) -> Result<NodeRuntime, RuntimeError> {
        let display_name = config.display_name.to_owned();
        Self::start_configured_guarded(config, events, |launch| launch(&display_name)).await
    }

    /// Prepare stores without launching producers; the host authorizes the final
    /// synchronous launch under its session guard and supplies the current name.
    pub async fn start_guarded(
        base_dir: &Path,
        account_id: &str,
        display_name: &str,
        password: &str,
        discovery_port: u16,
        on_dm: impl Fn(ReceivedDm) + Send + 'static,
        on_channel: impl Fn(crate::node::channel::ReceivedChannelMessage) + Send + 'static,
        on_file: impl Fn(crate::node::filebook::ReceivedFile) + Send + 'static,
        on_profile: impl Fn(crate::node::ReceivedProfile) + Send + 'static,
        on_call_signal: impl Fn(crate::node::ReceivedCallSignal) + Send + 'static,
        authorize_launch: impl FnOnce(
            &mut dyn FnMut(&str) -> Result<NodeRuntime, RuntimeError>,
        ) -> Result<NodeRuntime, RuntimeError>,
    ) -> Result<NodeRuntime, RuntimeError> {
        Self::start_configured_guarded(
            RuntimeConfig {
                base_dir,
                account_id,
                display_name,
                password,
                discovery_port,
                post_office_enabled: true,
                candidate_source: None,
            },
            RuntimeEvents {
                on_dm: Box::new(on_dm),
                on_channel: Box::new(move |message| {
                    if !message.reception_only {
                        on_channel(message);
                    }
                }),
                on_file: Box::new(on_file),
                on_profile: Box::new(on_profile),
                on_call_signal: Box::new(on_call_signal),
            },
            authorize_launch,
        )
        .await
    }

    /// Prepare stores, then let the host authorize the final synchronous launch
    /// under its session guard. The host may supply the current display name.
    pub async fn start_configured_guarded(
        config: RuntimeConfig<'_>,
        events: RuntimeEvents,
        authorize_launch: impl FnOnce(
            &mut dyn FnMut(&str) -> Result<NodeRuntime, RuntimeError>,
        ) -> Result<NodeRuntime, RuntimeError>,
    ) -> Result<NodeRuntime, RuntimeError> {
        Self::start_configured_guarded_with_network(
            config,
            events,
            RuntimeNetworkOptions::default(),
            authorize_launch,
        )
        .await
    }

    /// Initialize network preferences before any discovery or delivery producer starts.
    pub async fn start_configured_guarded_with_network(
        config: RuntimeConfig<'_>,
        events: RuntimeEvents,
        network: RuntimeNetworkOptions,
        authorize_launch: impl FnOnce(
            &mut dyn FnMut(&str) -> Result<NodeRuntime, RuntimeError>,
        ) -> Result<NodeRuntime, RuntimeError>,
    ) -> Result<NodeRuntime, RuntimeError> {
        Self::start_configured_guarded_with_network_observed(
            config,
            events,
            network,
            |_| Ok(()),
            authorize_launch,
        )
        .await
    }

    /// Observe authenticated public identifiers before preparing durable stores.
    /// Rejecting the observation stops startup before any listener or store opens.
    pub async fn start_configured_guarded_with_network_observed(
        config: RuntimeConfig<'_>,
        events: RuntimeEvents,
        network: RuntimeNetworkOptions,
        observe_identity: impl FnOnce(RuntimeIdentity) -> Result<(), RuntimeError>,
        authorize_launch: impl FnOnce(
            &mut dyn FnMut(&str) -> Result<NodeRuntime, RuntimeError>,
        ) -> Result<NodeRuntime, RuntimeError>,
    ) -> Result<NodeRuntime, RuntimeError> {
        Self::start_configured_guarded_with_network_cost_observed(
            config,
            events,
            network,
            super::NetworkCost::Unrestricted,
            observe_identity,
            authorize_launch,
        )
        .await
    }

    /// Initialize a host's cost observation before participant producers start,
    /// without adding required fields to the established SDK configuration types.
    pub async fn start_configured_guarded_with_network_cost_observed(
        config: RuntimeConfig<'_>,
        events: RuntimeEvents,
        network: RuntimeNetworkOptions,
        network_cost: super::NetworkCost,
        observe_identity: impl FnOnce(RuntimeIdentity) -> Result<(), RuntimeError>,
        authorize_launch: impl FnOnce(
            &mut dyn FnMut(&str) -> Result<NodeRuntime, RuntimeError>,
        ) -> Result<NodeRuntime, RuntimeError>,
    ) -> Result<NodeRuntime, RuntimeError> {
        Self::start_owned(
            super::RuntimeStartup::new(config, events)
                .with_network(network)
                .with_network_cost(network_cost),
            observe_identity,
            authorize_launch,
        )
        .await
    }

    /// Construct with concrete startup ownership and optional handed profile
    /// lease. Completion joins real initializer work even if this future drops.
    pub async fn start_owned(
        startup: super::RuntimeStartup<'_>,
        observe_identity: impl FnOnce(RuntimeIdentity) -> Result<(), RuntimeError>,
        authorize_launch: impl FnOnce(
            &mut dyn FnMut(&str) -> Result<NodeRuntime, RuntimeError>,
        ) -> Result<NodeRuntime, RuntimeError>,
    ) -> Result<NodeRuntime, RuntimeError> {
        let super::RuntimeStartup {
            config,
            events,
            network,
            network_cost,
            lease,
            work,
            root,
        } = startup;
        root.track(Self::start_profile_inner(
            config,
            events,
            network,
            network_cost,
            lease,
            work,
            observe_identity,
            authorize_launch,
        ))
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_profile_inner(
        config: RuntimeConfig<'_>,
        events: Option<RuntimeEvents>,
        network: RuntimeNetworkOptions,
        network_cost: super::NetworkCost,
        supplied_lease: Option<crate::storage::profile::ProfileStorageLease>,
        startup_work: Arc<super::runtime_work::RuntimeWork>,
        observe_identity: impl FnOnce(RuntimeIdentity) -> Result<(), RuntimeError>,
        authorize_launch: impl FnOnce(
            &mut dyn FnMut(&str) -> Result<NodeRuntime, RuntimeError>,
        ) -> Result<NodeRuntime, RuntimeError>,
    ) -> Result<NodeRuntime, RuntimeError> {
        let RuntimeConfig {
            base_dir,
            account_id,
            display_name,
            password,
            discovery_port,
            post_office_enabled,
            candidate_source,
        } = config;
        let host_owner = account_id.to_owned();
        let dir = base_dir.join("accounts").join(account_id);
        std::fs::create_dir_all(&dir)?;

        // The two keystore unlocks run sequential 600k-PBKDF2/Argon2 KDFs (~seconds of CPU,
        // the bulk of cold start). Offload them to the blocking pool so they don't monopolize
        // a tokio worker; the result is identical (same files, same ordering).
        let (identity, account, lease) = {
            let profile_dir = dir.clone();
            let permit = startup_work.admit().expect("startup root admitted");
            let identity_path = dir.join("identity.keystore");
            let account_path = dir.join("account.keystore");
            let password = password.to_string();
            permit
                .spawn_blocking(move || {
                    let lease = match supplied_lease {
                        Some(lease)
                            if lease.canonical_root() == std::fs::canonicalize(&profile_dir)? =>
                        {
                            lease
                        }
                        Some(_) => {
                            return Err(RuntimeError::Io(std::io::Error::other(
                                "supplied profile lease has another root",
                            )));
                        }
                        None => {
                            crate::storage::profile::ProfileStorageLease::acquire(&profile_dir)?
                        }
                    };
                    lease.validate_identity_sources(&identity_path)?;
                    let identity = keystore::load_or_create(&identity_path, &password)
                        .map_err(|e| RuntimeError::Open(e.into()))?;
                    let account = account_keystore::load_or_create(&account_path, &password)
                        .map_err(|e| RuntimeError::Open(e.into()))?;
                    Ok::<_, RuntimeError>((identity, account, lease))
                })
                .await
                .map_err(|e| RuntimeError::Io(std::io::Error::other(format!("join error: {e}"))))?
                .into_inner()?
        };
        let self_uid = identity.public().user_id();
        let account_id = account.account_id();
        observe_identity(RuntimeIdentity {
            device_id: self_uid.clone(),
            account_id: account_id.clone(),
        })?;

        // Dual-stack TCP listener (IPv6 + IPv4-mapped, falling back to IPv4) so the node is
        // reachable over IPv6/link-local too; OS-assigned port (0).
        let listener = super::privacy_routes::stable_listener(&dir).await?;
        let tcp_port = listener.local_addr()?.port();

        let roster: Arc<Mutex<Roster>> = Arc::new(Mutex::new(Roster::default()));
        let (targets, receivers) = if events.is_some() {
            let (incoming, incoming_rx) = mpsc::unbounded_channel();
            let (channel_incoming, channel_rx) = mpsc::unbounded_channel();
            let (file_incoming, file_rx) = mpsc::unbounded_channel();
            (
                Some(super::node::LegacyPayloadTargets {
                    incoming,
                    channel_incoming,
                    file_incoming,
                }),
                Some((incoming_rx, channel_rx, file_rx)),
            )
        } else {
            (None, None)
        };
        // Opening the node runs several more password KDFs (one per durable store). Offload
        // to the blocking pool so it doesn't monopolize a tokio worker during cold start;
        // `open_with_account` internally parallelizes those KDFs across scoped threads, so
        // the behavior (and result) is identical — just off the reactor.
        // Open the durable name directory in parallel with the node (its own password KDF),
        // so it doesn't add a serial step to cold start.
        let (node, names) = {
            let roster = Arc::clone(&roster);
            let messages_log = dir.join("messages.log");
            let sent_log = dir.join("sent.log");
            let names_path = dir.join("names.log");
            let password = password.to_string();
            let initial_display_name = display_name.to_owned();
            let permit = startup_work.admit().expect("startup root admitted");
            permit
                .spawn_blocking(move || {
                    let storage =
                        crate::storage::profile::context::ProfileStorage::open_participant(
                            lease,
                            &identity.public(),
                            &account.public(),
                            &messages_log,
                            &sent_log,
                            &password,
                        )?;
                    // Scoped children finish before this owner can return or unwind.
                    // Both results are joined before either failure is propagated.
                    let (node, names) = std::thread::scope(|scope| {
                        let names_storage = storage.clone();
                        let names_password = &password;
                        let names = scope.spawn(move || {
                            #[cfg(test)]
                            if let Some(hook) = NAMES_OPEN_HOOKS.lock().unwrap().remove(&names_path)
                            {
                                hook();
                            }
                            NameDirectory::open_in_storage(
                                &names_path,
                                names_password,
                                names_storage,
                            )
                        });
                        let node = scope.spawn(|| {
                            let node = Node::open_in_storage_delivery(
                                storage.clone(),
                                super::BoundedNodeOpenRequest {
                                    identity,
                                    account,
                                    roster,
                                    log_path: &messages_log,
                                    sent_path: &sent_log,
                                    password: &password,
                                },
                                targets,
                            )?;
                            if node.account_profile(&node.account_id())?.is_none()
                                && node.linked_devices().len() == 1
                            {
                                node.set_profile_display_name(&initial_display_name)
                                    .map_err(|e| {
                                        LogError::Io(std::io::Error::other(e.to_string()))
                                    })?;
                            }
                            Ok::<_, LogError>(node)
                        });
                        (node.join(), names.join())
                    });
                    let node = node.unwrap_or_else(|panic| std::panic::resume_unwind(panic))?;
                    let names = names.unwrap_or_else(|panic| std::panic::resume_unwind(panic))?;
                    Ok::<_, LogError>((node, names))
                })
                .await
                .map_err(|e| RuntimeError::Io(std::io::Error::other(format!("join error: {e}"))))?
                .into_inner()?
        };
        node.set_post_office_permissions(
            network.post_office_send_enabled,
            network.post_office_receive_enabled,
        );
        node.set_post_office_enabled(post_office_enabled);
        node.initialize_physical_lan_only(network.physical_lan_only);
        node.set_network_cost(network_cost);
        let names = Arc::new(Mutex::new(names));
        // Load policy and verified account proofs before discovery or accept/drain tasks.
        let visibility = Arc::new(crate::discovery::visibility::DiscoveryVisibility::new(
            false,
        ));
        {
            let node = node.clone();
            let directory = dir.clone();
            let password = password.to_string();
            let own = node.signed_announce(display_name, tcp_port);
            let visibility = visibility.clone();
            let permit = startup_work.admit().expect("startup root admitted");
            permit
                .spawn_blocking(move || {
                    #[cfg(test)]
                    if let Some(hook) = PRIVACY_OPEN_HOOKS.lock().unwrap().remove(&directory) {
                        hook();
                    }
                    node.configure_privacy(&directory, &password, &own, visibility)
                })
                .await
                .map_err(|_| {
                    RuntimeError::Io(std::io::Error::other("privacy initialization failed"))
                })?
                .into_inner()?;
        }
        visibility
            .set_public(!node.privacy_snapshot().invisible)
            .await;

        let prepared = PreparedParticipant::prepare(
            Arc::clone(&node),
            listener,
            visibility,
            ParticipantConfig {
                discovery_port,
                candidate_source,
            },
        )?;

        let mut launch = Some(move |current_name: &str| {
            // A persisted account claim survives host-local nicknames and adoption.
            // A rename committed while startup prepared still takes precedence.
            let canonical_name = node
                .account_profile(&node.account_id())?
                .map(|card| card.display_name)
                .filter(|name| {
                    crate::identity::user::User::validate_display_name(name).as_ref() == Some(name)
                });
            let current_name = if current_name == display_name {
                canonical_name.as_deref().unwrap_or(current_name)
            } else {
                current_name
            };
            let mut participant = prepared.launch(current_name);
            let mut tasks: Vec<JoinHandle<()>> = Vec::new();
            if let Some(events) = events {
                let RuntimeEvents {
                    on_dm,
                    on_channel,
                    on_file,
                    on_profile,
                    on_call_signal,
                } = events;
                let (mut incoming_rx, mut channel_rx, mut file_rx) =
                    receivers.expect("legacy delivery receivers");
                tasks.push(tokio::spawn(async move {
                    while let Some(dm) = incoming_rx.recv().await {
                        on_dm(dm);
                    }
                }));
                tasks.push(tokio::spawn(async move {
                    while let Some(message) = channel_rx.recv().await {
                        on_channel(message);
                    }
                }));
                tasks.push(tokio::spawn(async move {
                    while let Some(file) = file_rx.recv().await {
                        on_file(file);
                    }
                }));
                if let Some(mut receiver) = node.take_profile_receiver() {
                    tasks.push(tokio::spawn(async move {
                        while let Some(profile) = receiver.recv().await {
                            on_profile(profile);
                        }
                    }));
                }
                if let Some(mut receiver) = node.take_call_signal_receiver() {
                    tasks.push(tokio::spawn(async move {
                        while let Some(signal) = receiver.recv().await {
                            on_call_signal(signal);
                        }
                    }));
                }
            }
            {
                let node = node.clone();
                tasks.push(tokio::spawn(async move {
                    node.run_account_access_loop().await;
                }));
            }
            // Publish to new peers and retry failed or outdated profile publications.
            // Bounded by the live roster; no-op when neither public profile exists.
            {
                let node = Arc::clone(&node);
                let roster = Arc::clone(&roster);
                let names = Arc::clone(&names);
                tasks.push(tokio::spawn(async move {
                    // Futures stay owned by this task, so retirement drops their
                    // channels together with the rest of the runtime's work.
                    let mut published: std::collections::HashMap<String, ProfilePublication> =
                        std::collections::HashMap::new();
                    let mut in_flight = std::collections::HashSet::new();
                    let mut attempts: FuturesUnordered<BoxFuture<'static, (String, u64, bool)>> =
                        FuturesUnordered::new();
                    let mut poll = tokio::time::interval(Duration::from_secs(PROFILE_REPUBLISH_INTERVAL_SECS));
                    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        tokio::select! {
                            Some((uid, revision, complete)) = attempts.next(), if !attempts.is_empty() => {
                                in_flight.remove(&uid);
                                if let Some(publication) = published.get_mut(&uid)
                                    && publication.revision == revision
                                {
                                    publication.complete = complete;
                                }
                            }
                            _ = poll.tick() => {
                                let mut peers: Vec<PeerRecord> = {
                                    let r = roster.lock().expect("roster mutex not poisoned");
                                    r.peers()
                                };
                                // Remember names independently of slow network exchanges.
                                {
                                    let mut dir = names.lock().expect("names mutex not poisoned");
                                    peers.sort_by_key(|peer| peer.last_seen);
                                    for p in &peers {
                                        let _ = dir.record_peer_name(
                                            &p.public.user_id(),
                                            if p.post_office { None } else { p.account_id.as_deref() },
                                            &p.name,
                                        );
                                    }
                                }
                                let revision = match node.public_profile_revision() { Ok(Some(revision)) => revision, Ok(None) => continue, Err(error) => { eprintln!("[profile] revision read failed: {error}"); continue; } };
                                for peer in profile_republish_targets(&mut published, &peers, revision) {
                                    if attempts.len() >= MAX_PROFILE_PUBLICATIONS { break; }
                                    let uid = peer.public.user_id();
                                    if in_flight.contains(&uid) { continue; }
                                    let conversation = match published.get(&uid) {
                                        Some(publication) if publication.revision == revision => publication.conversation,
                                        _ => {
                                            let conversation = match node.prepare_current_profile_delivery(&peer) { Ok(Some(conversation)) => conversation, Ok(None) => continue, Err(error) => { eprintln!("[profile] publication read failed: {error}"); continue; } };
                                            published.insert(uid.clone(), ProfilePublication { revision, conversation, complete: false, last_attempt: None });
                                            conversation
                                        }
                                    };
                                    in_flight.insert(uid.clone());
                                    published.get_mut(&uid).expect("prepared publication").last_attempt = Some(std::time::Instant::now());
                                    let node = Arc::clone(&node);
                                    attempts.push(Box::pin(async move {
                                        let complete = node.deliver_profile_to(&peer, conversation).await;
                                        (uid, revision, complete)
                                    }));
                                }
                            }
                        }
                    }
                }));
            }

            for task in tasks {
                participant.register_host_task(task);
            }

            Ok(NodeRuntime {
                node,
                roster,
                names,
                user_id: self_uid,
                account_id,
                host_owner,
                display_name: Mutex::new(current_name.to_string()),
                participant,
                account_path: dir.join("account.keystore"),
                password: password.to_string(),
            })
        });
        let runtime = authorize_launch(&mut |name| {
            launch.take().expect("runtime launch is single-use")(name)
        })?;
        // Authorization may supply a name changed while stores were opening.
        let has_profile = match runtime.node.account_profile(runtime.account_id()) {
            Ok(profile) => profile.is_some(),
            Err(error) => {
                runtime.stop().await;
                return Err(error.into());
            }
        };
        if (has_profile
            || runtime.node.linked_devices().len() == 1
            || runtime.display_name() != display_name)
            && let Err(error) = runtime
                .node
                .set_profile_display_name(&runtime.display_name())
        {
            runtime.stop().await;
            return Err(RuntimeError::Open(LogError::Io(std::io::Error::other(
                error.to_string(),
            ))));
        }
        Ok(runtime)
    }

    /// This node's own user-id fingerprint.
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    pub fn discovery_port(&self) -> u16 {
        self.participant.discovery_port()
    }

    /// Probe a user-supplied address without revealing this device's identity
    /// until the other end returns a fresh, signed Mesh Talk response.
    pub async fn probe_endpoint(&self, endpoint: std::net::SocketAddr) -> std::io::Result<()> {
        self.participant.probe_endpoint(endpoint).await
    }

    pub async fn probe_endpoint_if(
        &self,
        endpoint: std::net::SocketAddr,
        authorize: impl FnMut(&mut dyn FnMut() -> std::io::Result<()>) -> std::io::Result<()> + Send,
    ) -> std::io::Result<()> {
        self.participant
            .probe_endpoint_if(endpoint, authorize)
            .await
    }

    /// The display name this node advertises to peers.
    pub fn display_name(&self) -> String {
        self.display_name.lock().expect("display name lock").clone()
    }

    /// Change the display name this node advertises, live: re-sign the announce with the
    /// node's device key (keys never leave the node), swap it into the shared cell so the
    /// discovery loops broadcast it from their next send, and record our own name durably
    /// so it resolves consistently. Fires an immediate re-announce so peers converge fast.
    /// `account_id`/`user_id` are unchanged — only the human-readable name moves.
    pub fn set_display_name(&self, display_name: &str) {
        if let Err(error) = self.try_set_display_name(display_name) {
            log::warn!("Unable to update account profile: {error}");
        }
    }

    pub fn try_set_display_name(&self, display_name: &str) -> Result<(), NodeError> {
        let mut current = self.display_name.lock().expect("display name lock");
        self.node.set_profile_display_name(display_name)?;
        self.participant
            .set_display_name(display_name)
            .map_err(|error| {
                NodeError::InvalidInput(format!("unable to update presence: {error}"))
            })?;
        *current = display_name.to_string();
        // Keep our own name in the durable directory too (it otherwise only tracks peers),
        // so `display_name_for(own_id)` stays consistent. A no-op when unchanged.
        let _ = self
            .names
            .lock()
            .expect("names mutex not poisoned")
            .record(&self.user_id, display_name);
        Ok(())
    }

    /// The local TCP port the node's accept loop is listening on.
    pub fn listen_tcp_port(&self) -> u16 {
        self.participant.listen_tcp_port()
    }

    /// This node's cryptographic account id (stable across devices once linking
    /// is wired; today each device generates its own account on first run).
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    /// Local host account namespace used for this runtime's persistent stores.
    pub fn host_account_id(&self) -> Option<&str> {
        Some(&self.host_owner)
    }

    pub fn privacy_snapshot(&self) -> crate::node::PrivacySnapshot {
        self.node.privacy_snapshot()
    }
    pub async fn set_invisible(&self, invisible: bool) -> std::io::Result<()> {
        self.node.set_invisible(invisible).await
    }
    pub async fn set_allowed(&self, account: &str, allowed: bool) -> std::io::Result<()> {
        self.node.set_allowed(account, allowed).await
    }
    pub async fn initiate_contact(&self, account: &str) -> std::io::Result<()> {
        self.node.initiate_contact(account).await
    }
    pub async fn set_invisible_if(
        &self,
        invisible: bool,
        authorize: impl FnOnce() -> std::io::Result<()> + Send,
    ) -> std::io::Result<()> {
        self.node.set_invisible_if(invisible, authorize).await
    }
    pub async fn set_allowed_if(
        &self,
        account: &str,
        allowed: bool,
        authorize: impl FnOnce() -> std::io::Result<()> + Send,
    ) -> std::io::Result<()> {
        self.node.set_allowed_if(account, allowed, authorize).await
    }
    pub async fn initiate_contact_if(
        &self,
        account: &str,
        authorize: impl FnOnce() -> std::io::Result<()> + Send,
    ) -> std::io::Result<()> {
        self.node.initiate_contact_if(account, authorize).await
    }

    /// Force an immediate re-announce + /24 rescan, on top of the periodic timers.
    /// Useful when LAN discovery is flaky (e.g. first-contact lost to UDP drops):
    /// the user can press "announce now" to converge without waiting for the next
    /// tick. Idempotent and harmless — peers self-filter duplicate announces.
    pub fn trigger_discovery(&self) {
        self.participant.trigger_discovery();
    }

    /// Whether this runtime's discovery producers are still running.
    pub fn discovery_available(&self) -> bool {
        self.participant.discovery_available()
    }

    /// A snapshot of currently-known peers.
    pub fn peers(&self) -> Vec<PeerRecord> {
        match self.node.cached_peer_snapshot() {
            Ok(peers) => peers,
            Err(_) => {
                log::warn!("verified discovery cache update failed");
                Vec::new()
            }
        }
    }

    /// Account conversations retained locally, including contacts with no
    /// currently discoverable device.
    pub fn known_account_ids(&self) -> Result<Vec<String>, NodeError> {
        self.node.known_account_ids()
    }

    /// Verified historical names for offline account rows; never supplies presence.
    pub fn known_account_names(
        &self,
    ) -> Result<std::collections::HashMap<String, Vec<String>>, LogError> {
        let proofs = self.node.historical_dm_peers();
        // Record actual observed renames before projecting any UI surface. Old,
        // unchanged sibling announcements cannot displace the latest account name.
        let mut live = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .peers();
        live.sort_by_key(|peer| peer.last_seen);
        let live_bindings: std::collections::HashMap<_, _> = live
            .iter()
            .map(|peer| {
                (
                    peer.public.user_id(),
                    (peer.public.clone(), peer.account_id.clone()),
                )
            })
            .collect();
        {
            let mut directory = self.names.lock().expect("names mutex not poisoned");
            for peer in live {
                if proofs.iter().any(|proof| proof.public() == peer.public) {
                    let _ = directory.record_peer_name(
                        &peer.public.user_id(),
                        peer.account_id.as_deref(),
                        &peer.name,
                    );
                }
            }
        }
        let mut names: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        let mut devices: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for proof in proofs {
            let Some(account) = proof.account_id() else {
                continue;
            };
            // A retained physical device may now announce a different account.
            // Its current nickname cannot rename the immutable historical principal.
            let same_account =
                live_bindings
                    .get(&proof.user_id)
                    .is_some_and(|(public, binding)| {
                        *public == proof.public() && binding.as_deref() == Some(account.as_str())
                    });
            let name = if same_account {
                self.display_name_for(&proof.user_id).unwrap_or(proof.name)
            } else {
                proof.name
            };
            if !name.is_empty() {
                if same_account {
                    devices
                        .entry(account.clone())
                        .or_default()
                        .push(proof.user_id);
                }
                names.entry(account).or_default().push(name);
            }
        }
        let directory = self.names.lock().expect("names mutex not poisoned");
        for (account, values) in &mut names {
            values.sort();
            values.dedup();
            if let Some(latest) = directory.name_for_account(account).or_else(|| {
                devices
                    .get(account)
                    .and_then(|users| directory.latest_name_for_users(users))
            }) {
                values.retain(|name| *name != latest);
                values.insert(0, latest);
            }
        }
        // This account's shared profile owns its nickname across its devices.
        // For contacts, retain the verified announced nickname above: legacy
        // public cards may still contain an old login/display label from before
        // nickname edits updated those claims. A stale card must not rename a
        // contact. Its name is a fallback only if no nickname is known.
        let mut after = None;
        loop {
            let page = self
                .node
                .profiles
                .lock()
                .expect("profiles mutex")
                .names_page(after.as_deref())?;
            if page.is_empty() {
                break;
            }
            after = page.last().map(|(account, _)| account.clone());
            for (account, display_name) in page {
                if crate::identity::user::User::validate_display_name(&display_name).as_ref()
                    == Some(&display_name)
                {
                    let values = names.entry(account.clone()).or_default();
                    if account == self.account_id() {
                        values.retain(|name| *name != display_name);
                        values.insert(0, display_name);
                    } else if values.is_empty() {
                        values.push(display_name);
                    }
                }
            }
        }
        Ok(names)
    }

    /// The display name for a device `user_id`: the live roster's (freshest) if the peer is
    /// currently around, else the last name we durably recorded — so a member that went
    /// offline (and was evicted from the roster) still resolves to a name instead of a raw id.
    pub fn display_name_for(&self, user_id: &str) -> Option<String> {
        let live = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .get(user_id)
            .map(|p| p.name.clone())
            .filter(|n| !n.is_empty());
        live.or_else(|| {
            self.names
                .lock()
                .expect("names mutex not poisoned")
                .name_for_user(user_id)
        })
    }

    /// The public identity of a known peer (to derive its DM conversation), if known.
    pub fn peer_public(&self, user_id: &str) -> Option<PublicIdentity> {
        self.node.routing_peer(user_id).map(|p| p.public)
    }

    /// Resolve a known chat peer for channel membership. A post office is a
    /// transport node, not a participant that can accept a channel invitation.
    pub fn channel_invitee_public(&self, user_id: &str) -> Option<PublicIdentity> {
        self.node
            .routing_peer(user_id)
            .and_then(channel_invitee_identity)
    }

    /// Send a DM to a known peer by user-id.
    pub async fn send_dm(&self, recipient: &str, text: &[u8]) -> Result<(), NodeError> {
        self.node.send_dm(recipient, text).await
    }

    /// The last `limit` messages of the DM with `peer`, both directions.
    pub fn history(
        &self,
        peer: &PublicIdentity,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>, super::NodeError> {
        self.node.dm_history(peer, limit)
    }

    /// Send a DM addressed to an account (fan-out to its devices + self-sync to ours).
    pub async fn send_to_account(
        &self,
        target_account_id: &str,
        text: &[u8],
        reply_to: Option<crate::eventlog::event::EventId>,
    ) -> Result<(), NodeError> {
        self.node
            .send_to_account(target_account_id, text, reply_to)
            .await
    }

    /// Process-local revision for durable account history projection refreshes.
    pub fn account_history_revision(&self) -> u64 {
        self.node.account_history_revision()
    }

    /// Account-level conversation history with `peer_account_id`.
    pub fn account_history(
        &self,
        peer_account_id: &str,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>, super::NodeError> {
        self.node.account_history(peer_account_id, limit)
    }

    /// Set (or clear) this user's own avatar and propagate it to peers as a signed profile.
    pub async fn set_avatar(&self, avatar: Option<Vec<u8>>) -> Result<(), NodeError> {
        self.node.set_avatar(avatar).await
    }

    /// The avatar a peer account propagated to us (data-URL bytes), or `None`.
    pub fn peer_avatar(
        &self,
        account_id: &str,
    ) -> Result<Option<Vec<u8>>, crate::eventlog::LogError> {
        self.node.peer_avatar(account_id)
    }

    /// The session password held for re-encrypting the keystore — used only to
    /// re-spawn the runtime when adopting a freshly-linked account (no re-login).
    /// `pub` so the desktop shell can re-spawn after linking (it lives in another crate).
    pub fn restart_password(&self) -> &str {
        &self.password
    }

    /// Re-key: replace this device's account with a fresh one on disk and return the
    /// new account id. The caller adopts it by re-spawning the runtime. This abandons
    /// the old `account_id` — the honest response to a lost/compromised device in the
    /// shared-account v1 (a lost device keeps the OLD secret, so the only way to lock it
    /// out is to move to a new identity and re-link the devices you still trust). It is
    /// NOT a silent revocation: contacts re-discover you under the new account, and your
    /// other good devices must be re-linked.
    pub fn rekey_account(&self) -> Result<String, NodeError> {
        let account = crate::identity::account::Account::generate();
        self.node.persist_account_adoption(
            &account.public(),
            || {
                crate::identity::account_keystore::save(
                    &self.account_path,
                    &self.password,
                    &account,
                )
                .map_err(|e| NodeError::Channel(format!("persist new account: {e}")))
            },
            || {
                crate::identity::account_keystore::load(&self.account_path, &self.password)
                    .map(|account| account.public())
                    .map_err(|e| NodeError::Channel(e.to_string()))
            },
        )?;
        Ok(account.account_id())
    }

    /// Enter "link a device" mode; returns the one-time code to display.
    pub fn start_linking(&self) -> String {
        self.node.start_linking()
    }

    /// Leave "link a device" mode.
    pub fn stop_linking(&self) {
        self.node.stop_linking()
    }

    /// Link THIS device to an existing device `peer_user_id` using `code`. On success
    /// persists the adopted account secret to disk (effective next login) and returns
    /// the adopted account id.
    pub async fn link_device(&self, peer_user_id: &str, code: &str) -> Result<String, NodeError> {
        self.link_device_if(peer_user_id, code, |commit| commit())
            .await
    }

    pub async fn link_device_if(
        &self,
        peer_user_id: &str,
        code: &str,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<String, NodeError> {
        self.link_device_version_if(peer_user_id, code, false, |_| Ok(()), &mut authorize)
            .await
    }
    pub async fn link_device_with_account_access_if(
        &self,
        peer_user_id: &str,
        code: &str,
        authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError> + Send,
    ) -> Result<String, NodeError> {
        self.link_device_with_account_access_checked_if(peer_user_id, code, |_| Ok(()), authorize)
            .await
    }
    pub async fn link_device_with_account_access_checked_if(
        &self,
        peer_user_id: &str,
        code: &str,
        preflight: impl FnMut(&super::AccountAccessSnapshot) -> Result<(), NodeError> + Send,
        authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError> + Send,
    ) -> Result<String, NodeError> {
        self.link_device_version_if(peer_user_id, code, true, preflight, authorize)
            .await
    }
    async fn link_device_version_if(
        &self,
        peer_user_id: &str,
        code: &str,
        access_required: bool,
        preflight: impl FnMut(&super::AccountAccessSnapshot) -> Result<(), NodeError> + Send,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<String, NodeError> {
        let peer = {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            roster
                .get(peer_user_id)
                .cloned()
                .ok_or_else(|| NodeError::UnknownPeer(peer_user_id.to_string()))?
        };
        let linked = if access_required {
            self.node
                .link_to_device_with_account_access_checked_if(
                    peer.addr,
                    &peer.public,
                    code,
                    preflight,
                    &mut authorize,
                )
                .await?
        } else {
            self.node
                .link_to_device_if(peer.addr, &peer.public, code, &mut authorize)
                .await?
        };
        if access_required && peer.account_id.as_deref() != Some(linked.account_id.as_str()) {
            return Err(NodeError::Linking(super::LinkingError::InvalidResponse));
        }
        let mut adopted = false;
        authorize(&mut || {
            let account = crate::identity::account::Account::from_secret_bytes_with_created_at(
                linked.secret,
                linked.created_at_ms,
            )
            .ok_or_else(|| NodeError::InvalidInput("invalid linked creation metadata".into()))?;
            let device_dir = self
                .account_path
                .parent()
                .ok_or_else(|| NodeError::Channel("account path has no parent".into()))?;
            let mut devices = super::account_devices::AccountDevices::open(
                &super::account_devices::file_path(device_dir, &linked.account_id),
                &self.password,
                &linked.account_id,
            )
            .map_err(NodeError::Log)?;
            for public in [self.node.identity.public(), peer.public.clone()] {
                devices
                    .insert(super::account_devices::AccountDevice {
                        certificate: account.certify(&public.ed25519_pub),
                        public,
                    })
                    .map_err(NodeError::Log)?;
            }
            self.node.prepare_account_transition(&account)?;
            self.node.persist_account_adoption(
                &account.public(),
                || {
                    crate::identity::account_keystore::save(
                        &self.account_path,
                        &self.password,
                        &account,
                    )
                    .map_err(|e| NodeError::Channel(format!("persist linked account: {e}")))
                },
                || {
                    crate::identity::account_keystore::load(&self.account_path, &self.password)
                        .map(|account| account.public())
                        .map_err(|e| NodeError::Channel(e.to_string()))
                },
            )?;
            adopted = true;
            Ok(())
        })?;
        if !adopted {
            return Err(NodeError::Authorization(
                "account adoption was not authorized".into(),
            ));
        }
        Ok(linked.account_id)
    }

    /// Create a legacy device-only channel; returns its conversation id.
    /// Account-based desktop creation uses `Node::create_account_channel_invited_if`.
    pub async fn create_channel(
        &self,
        name: &str,
        members: Vec<PublicIdentity>,
    ) -> Result<crate::eventlog::event::ConversationId, NodeError> {
        self.node.create_channel(name, members).await
    }

    /// Channels this node is a member of.
    pub fn list_channels(&self) -> Result<Vec<crate::node::ChannelSummary>, NodeError> {
        self.node.list_channels()
    }

    pub fn channel_owner_account_id(
        &self,
        channel: crate::eventlog::event::ConversationId,
    ) -> Option<String> {
        self.node.channel_owner_account_id(channel)
    }
    pub fn channel_can_manage(
        &self,
        channel: crate::eventlog::event::ConversationId,
    ) -> Result<bool, NodeError> {
        self.node.channel_can_manage(channel)
    }

    pub fn channel_can_send(
        &self,
        channel: crate::eventlog::event::ConversationId,
    ) -> Result<bool, NodeError> {
        self.node.channel_can_send(channel)
    }

    pub fn channel_member_account_binding(
        &self,
        channel: crate::eventlog::event::ConversationId,
        member: &PublicIdentity,
    ) -> Option<String> {
        self.node.channel_member_account_binding(channel, member)
    }

    /// The current members of a channel (empty if unknown).
    pub fn channel_members(
        &self,
        channel: crate::eventlog::event::ConversationId,
    ) -> Vec<PublicIdentity> {
        self.node.channel_members(channel)
    }

    pub fn channel_replica_enabled(
        &self,
        channel: crate::eventlog::event::ConversationId,
        member: &str,
    ) -> bool {
        self.node.channel_replica_enabled(channel, member)
    }

    pub fn channel_pending_members(
        &self,
        channel: crate::eventlog::event::ConversationId,
    ) -> Vec<String> {
        self.node.channel_pending_members(channel)
    }

    pub fn channel_activity(
        &self,
        channel: crate::eventlog::event::ConversationId,
    ) -> Result<Vec<crate::node::ChannelActivity>, NodeError> {
        self.node.channel_activity(channel)
    }

    /// Resolve a member's certified account even after discovery expires.
    pub fn account_id_for_device(&self, device: &PublicIdentity) -> Option<String> {
        self.node.account_id_for_device(device)
    }

    /// The current owner of a channel.
    pub fn channel_owner(&self, channel: crate::eventlog::event::ConversationId) -> String {
        self.node.channel_owner(channel)
    }

    pub fn channel_recovery(
        &self,
        channel: crate::eventlog::event::ConversationId,
    ) -> Option<super::ChannelRecovery> {
        self.node.channel_recovery(channel)
    }

    /// History of a channel (all senders).
    pub fn channel_history(
        &self,
        channel: crate::eventlog::event::ConversationId,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>, super::NodeError> {
        self.node.channel_history(channel, limit)
    }

    /// Search all DMs + channels for `query`.
    pub fn search(&self, query: &str) -> Result<Vec<crate::node::SearchHit>, super::NodeError> {
        self.node.search(query)
    }

    /// Delete one message from THIS device only (local, not propagated). `account` selects
    /// the id space of `target` (account msg id vs event id).
    pub fn delete_message(
        &self,
        conv: crate::eventlog::event::ConversationId,
        target: crate::eventlog::event::EventId,
        account: bool,
    ) -> Result<usize, NodeError> {
        self.node.delete_message(conv, target, account)
    }

    /// Clear all locally-stored history for a conversation (text + files, both directions).
    pub fn clear_conversation(
        &self,
        conv: crate::eventlog::event::ConversationId,
    ) -> Result<usize, NodeError> {
        self.node.clear_conversation(conv)
    }

    /// Retention prune: erase every locally-stored message older than `cutoff_ms`.
    pub fn prune_older_than(&self, cutoff_ms: u64) -> Result<usize, NodeError> {
        self.node.prune_older_than(cutoff_ms)
    }

    /// Recall (unsend) one of our own account messages within the recall window.
    pub async fn recall_account(
        &self,
        target_account_id: &str,
        target: crate::eventlog::event::EventId,
    ) -> Result<(), NodeError> {
        self.node.recall_account(target_account_id, target).await
    }

    /// Recall (unsend) one of our own channel messages within the recall window.
    pub async fn recall_channel(
        &self,
        channel: crate::eventlog::event::ConversationId,
        target: crate::eventlog::event::EventId,
    ) -> Result<(), NodeError> {
        self.node.recall_channel(channel, target).await
    }

    /// Send an animated sticker to an account (fan-out + self-sync, like a DM).
    pub async fn send_sticker_to_account(
        &self,
        target_account_id: &str,
        sticker_id: &str,
        fallback: &[u8],
    ) -> Result<(), NodeError> {
        self.node
            .send_sticker_to_account(target_account_id, sticker_id, fallback)
            .await
    }

    /// Send an animated sticker to a channel.
    pub async fn send_sticker_channel(
        &self,
        channel: crate::eventlog::event::ConversationId,
        sticker_id: &str,
        fallback: &[u8],
    ) -> Result<(), NodeError> {
        self.node
            .send_sticker_channel(channel, sticker_id, fallback)
            .await
    }

    /// Aggregated reactions in the DM with `peer`.
    pub fn reactions_dm(
        &self,
        peer: &PublicIdentity,
    ) -> Result<Vec<crate::node::reaction::ReactionView>, NodeError> {
        self.node.reactions_dm(peer)
    }

    /// Aggregated reactions in a channel.
    pub fn channel_reactions(
        &self,
        channel: crate::eventlog::event::ConversationId,
    ) -> Result<Vec<crate::node::reaction::ReactionView>, NodeError> {
        self.node.reactions(channel)
    }

    /// Aggregated reactions in the account conversation with `peer_account_id`.
    pub fn account_reactions(
        &self,
        peer_account_id: &str,
    ) -> Result<Vec<crate::node::reaction::ReactionView>, NodeError> {
        self.node.account_reactions(peer_account_id)
    }

    /// A cloned handle to the underlying node, so an IPC command can snapshot it
    /// and release the `NodeState` lock before an async send.
    pub fn handle(&self) -> Arc<Node> {
        Arc::clone(&self.node)
    }
}

#[cfg(test)]
#[path = "runtime_privacy_tests.rs"]
mod privacy_tests;

#[cfg(test)]
#[path = "runtime_retirement_tests.rs"]
mod retirement_tests;

#[cfg(test)]
#[path = "runtime_routed_tests.rs"]
mod tests;
