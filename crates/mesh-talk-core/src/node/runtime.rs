//! The desktop App context for the node: opens a per-user `Node`, starts
//! its network loops (discovery, accept, drain) and an inbound-DM forwarder, and
//! holds the handles. Tauri-agnostic — inbound DMs are delivered via an injected
//! `on_dm` callback, so this is unit-testable without a GUI.

use super::{ParticipantConfig, ParticipantRuntime, PreparedParticipant};
use crate::discovery::candidates::CandidateSource;
use crate::discovery::{PeerRecord, Roster};
use crate::eventlog::LogError;
use crate::identity::account_keystore;
use crate::identity::device::PublicIdentity;
use crate::identity::keystore;
use crate::node::name_directory::NameDirectory;
use crate::node::{HistoryEntry, Node, NodeError, ReceivedDm};
#[cfg(test)]
use crate::transport::net::routed_ipv6_socket;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// How often the node checks for freshly-discovered peers and re-publishes its current
/// avatar to them (so a new contact sees the photo without waiting for the next change).
/// A no-op when no new peers appeared and when we've never set an avatar.
const PROFILE_REPUBLISH_INTERVAL_SECS: u64 = 3;

#[cfg(test)]
type InitializationHook = Box<dyn FnOnce() + Send>;
#[cfg(test)]
static NAMES_OPEN_HOOKS: std::sync::LazyLock<
    Mutex<std::collections::HashMap<std::path::PathBuf, InitializationHook>>,
> = std::sync::LazyLock::new(Default::default);

/// Which peers to (re)publish our profile to this round: those present now (excluding post
/// offices, which relay rather than render avatars) that were ABSENT in the previous round.
/// `present_prev` holds the PREVIOUS round's present set and is updated in place to this
/// round's. Using a per-round present set — rather than a monotonic "ever seen" set — means a
/// peer that goes offline (evicted after PEER_TTL) and reconnects is treated as new again and
/// gets our current avatar, instead of being stuck on whatever it had before it restarted.
fn profile_republish_targets(
    present_prev: &mut std::collections::HashSet<String>,
    peers: &[PeerRecord],
) -> Vec<PeerRecord> {
    let mut present_now = std::collections::HashSet::new();
    let mut targets = Vec::new();
    for p in peers {
        if p.post_office {
            continue;
        }
        let uid = p.public.user_id();
        if !present_prev.contains(&uid) {
            targets.push(p.clone());
        }
        present_now.insert(uid);
    }
    *present_prev = present_now;
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

impl std::error::Error for RuntimeError {}

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
    /// The display name this node advertises (for the diagnostics page).
    display_name: String,
    participant: ParticipantRuntime,
    /// Where the account keystore lives, so a successful device link can persist the
    /// adopted account secret (effective on next login).
    account_path: std::path::PathBuf,
    /// The session password, to re-encrypt the keystore when adopting a linked account.
    password: String,
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
        self.participant.stop().await;
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
            |launch| Ok(launch(display_name)),
        )
        .await
    }

    /// Start a node using named host settings and inbound event handlers.
    pub async fn start_configured(
        config: RuntimeConfig<'_>,
        events: RuntimeEvents,
    ) -> Result<NodeRuntime, RuntimeError> {
        let display_name = config.display_name.to_owned();
        Self::start_configured_guarded(config, events, |launch| Ok(launch(&display_name))).await
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
            &mut dyn FnMut(&str) -> NodeRuntime,
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
            &mut dyn FnMut(&str) -> NodeRuntime,
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
            &mut dyn FnMut(&str) -> NodeRuntime,
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
        let RuntimeEvents {
            on_dm,
            on_channel,
            on_file,
            on_profile,
            on_call_signal,
        } = events;
        let dir = base_dir.join("accounts").join(account_id);
        std::fs::create_dir_all(&dir)?;

        // The two keystore unlocks run sequential 600k-PBKDF2/Argon2 KDFs (~seconds of CPU,
        // the bulk of cold start). Offload them to the blocking pool so they don't monopolize
        // a tokio worker; the result is identical (same files, same ordering).
        let (identity, account) = {
            let identity_path = dir.join("identity.keystore");
            let account_path = dir.join("account.keystore");
            let password = password.to_string();
            tokio::task::spawn_blocking(move || {
                let identity = keystore::load_or_create(&identity_path, &password)
                    .map_err(|e| RuntimeError::Open(e.into()))?;
                let account = account_keystore::load_or_create(&account_path, &password)
                    .map_err(|e| RuntimeError::Open(e.into()))?;
                Ok::<_, RuntimeError>((identity, account))
            })
            .await
            .map_err(|e| RuntimeError::Io(std::io::Error::other(format!("join error: {e}"))))??
        };
        let self_uid = identity.public().user_id();
        let account_id = account.account_id();

        // Dual-stack TCP listener (IPv6 + IPv4-mapped, falling back to IPv4) so the node is
        // reachable over IPv6/link-local too; OS-assigned port (0).
        let listener = super::privacy_routes::stable_listener(&dir).await?;
        let tcp_port = listener.local_addr()?.port();

        let roster: Arc<Mutex<Roster>> = Arc::new(Mutex::new(Roster::default()));
        let (incoming_tx, mut incoming_rx) = mpsc::unbounded_channel::<ReceivedDm>();
        let (channel_tx, mut channel_rx) =
            mpsc::unbounded_channel::<crate::node::channel::ReceivedChannelMessage>();
        let (file_tx, mut file_rx) =
            mpsc::unbounded_channel::<crate::node::filebook::ReceivedFile>();
        // Opening the node runs several more password KDFs (one per durable store). Offload
        // to the blocking pool so it doesn't monopolize a tokio worker during cold start;
        // `open_with_account` internally parallelizes those KDFs across scoped threads, so
        // the behavior (and result) is identical — just off the reactor.
        // Open the durable name directory in parallel with the node (its own password KDF),
        // so it doesn't add a serial step to cold start.
        let names_handle = {
            let names_path = dir.join("names.log");
            let password = password.to_string();
            tokio::task::spawn_blocking(move || {
                #[cfg(test)]
                {
                    let hook = NAMES_OPEN_HOOKS.lock().unwrap().remove(&names_path);
                    if let Some(hook) = hook {
                        hook();
                    }
                }
                NameDirectory::open(&names_path, &password)
            })
        };
        let node_result = {
            let roster = Arc::clone(&roster);
            let messages_log = dir.join("messages.log");
            let sent_log = dir.join("sent.log");
            let password = password.to_string();
            tokio::task::spawn_blocking(move || {
                Node::open_with_account(
                    identity,
                    account,
                    roster,
                    incoming_tx,
                    channel_tx,
                    file_tx,
                    &messages_log,
                    &sent_log,
                    &password,
                )
            })
            .await
        };
        // Both blocking writers must finish even when either initializer fails.
        // Dropping a JoinHandle would detach a writer into the replacement profile.
        let names_result = names_handle.await;
        let node = node_result
            .map_err(|e| RuntimeError::Io(std::io::Error::other(format!("join error: {e}"))))??;
        node.set_post_office_permissions(
            network.post_office_send_enabled,
            network.post_office_receive_enabled,
        );
        node.set_post_office_enabled(post_office_enabled);
        node.initialize_physical_lan_only(network.physical_lan_only);
        let names = Arc::new(Mutex::new(names_result.map_err(|e| {
            RuntimeError::Io(std::io::Error::other(format!("join error: {e}")))
        })??));
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
            tokio::task::spawn_blocking(move || {
                node.configure_privacy(&directory, &password, &own, visibility)
            })
            .await
            .map_err(|_| {
                RuntimeError::Io(std::io::Error::other("privacy initialization failed"))
            })??;
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
            let mut participant = prepared.launch(current_name);
            let mut tasks: Vec<JoinHandle<()>> = Vec::new();
            tasks.push(tokio::spawn(async move {
                while let Some(dm) = incoming_rx.recv().await {
                    on_dm(dm);
                }
            }));
            tasks.push(tokio::spawn(async move {
                while let Some(msg) = channel_rx.recv().await {
                    on_channel(msg);
                }
            }));
            tasks.push(tokio::spawn(async move {
                while let Some(f) = file_rx.recv().await {
                    on_file(f);
                }
            }));
            // Forward received peer profiles (avatar updates) to the host app.
            if let Some(mut profile_rx) = node.take_profile_receiver() {
                tasks.push(tokio::spawn(async move {
                    while let Some(p) = profile_rx.recv().await {
                        on_profile(p);
                    }
                }));
            }
            // Forward inbound call signals (WebRTC SDP/bye) to the host app — ephemeral, never logged.
            if let Some(mut call_signal_rx) = node.take_call_signal_receiver() {
                tasks.push(tokio::spawn(async move {
                    while let Some(s) = call_signal_rx.recv().await {
                        on_call_signal(s);
                    }
                }));
            }
            // Re-publish our avatar to freshly-discovered peers: poll the roster, and for any
            // peer user-id we hadn't seen before, send our current profile (a no-op if we've
            // set no avatar). Bounded — `seen` only tracks ids, and re-publish skips known peers.
            {
                let node = Arc::clone(&node);
                let roster = Arc::clone(&roster);
                let names = Arc::clone(&names);
                tasks.push(tokio::spawn(async move {
                    // The set of peers PRESENT in the previous poll — NOT a monotonic "ever seen"
                    // set. A peer is republished-to when it's present now but was absent last
                    // round, which covers both first discovery AND a reconnect after the roster
                    // evicted it (PEER_TTL). A monotonic set would never re-publish to a peer that
                    // restarted, so it would keep showing our stale avatar until we re-set it.
                    let mut present_prev: std::collections::HashSet<String> =
                        std::collections::HashSet::new();
                    loop {
                        let peers: Vec<PeerRecord> = {
                            let r = roster.lock().expect("roster mutex not poisoned");
                            r.peers()
                        };
                        // Remember every peer's announced name durably, so a member that later
                        // goes offline (and is evicted from the live roster) still shows a name.
                        // `record` is a no-op when unchanged, so this doesn't grow the log.
                        {
                            let mut dir = names.lock().expect("names mutex not poisoned");
                            for p in &peers {
                                let _ = dir.record(&p.public.user_id(), &p.name);
                            }
                        }
                        // Re-publish our avatar to peers that just (re)appeared (a no-op if we've
                        // set no avatar). Bounded by the live roster size.
                        for peer in profile_republish_targets(&mut present_prev, &peers) {
                            node.republish_profile_to(&peer).await;
                        }
                        tokio::time::sleep(Duration::from_secs(PROFILE_REPUBLISH_INTERVAL_SECS))
                            .await;
                    }
                }));
            }

            for task in tasks {
                participant.register_host_task(task);
            }

            NodeRuntime {
                node,
                roster,
                names,
                user_id: self_uid,
                account_id,
                display_name: current_name.to_string(),
                participant,
                account_path: dir.join("account.keystore"),
                password: password.to_string(),
            }
        });
        authorize_launch(&mut |name| launch.take().expect("runtime launch is single-use")(name))
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
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Change the display name this node advertises, live: re-sign the announce with the
    /// node's device key (keys never leave the node), swap it into the shared cell so the
    /// discovery loops broadcast it from their next send, and record our own name durably
    /// so it resolves consistently. Fires an immediate re-announce so peers converge fast.
    /// `account_id`/`user_id` are unchanged — only the human-readable name moves.
    pub fn set_display_name(&mut self, display_name: &str) {
        if self.participant.set_display_name(display_name).is_err() {
            log::warn!("Unable to update node presence");
            return;
        }
        self.display_name = display_name.to_string();
        // Keep our own name in the durable directory too (it otherwise only tracks peers),
        // so `display_name_for(own_id)` stays consistent. A no-op when unchanged.
        let _ = self
            .names
            .lock()
            .expect("names mutex not poisoned")
            .record(&self.user_id, display_name);
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
        self.account_path.parent()?.file_name()?.to_str()
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
    pub fn known_account_ids(&self) -> Vec<String> {
        self.node.known_account_ids()
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
    pub fn history(&self, peer: &PublicIdentity, limit: usize) -> Vec<HistoryEntry> {
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
    pub fn account_history(&self, peer_account_id: &str, limit: usize) -> Vec<HistoryEntry> {
        self.node.account_history(peer_account_id, limit)
    }

    /// Set (or clear) this user's own avatar and propagate it to peers as a signed profile.
    pub async fn set_avatar(&self, avatar: Option<Vec<u8>>) -> Result<(), NodeError> {
        self.node.set_avatar(avatar).await
    }

    /// The avatar a peer account propagated to us (data-URL bytes), or `None`.
    pub fn peer_avatar(&self, account_id: &str) -> Option<Vec<u8>> {
        self.node.peer_avatar(account_id)
    }

    /// Every peer avatar we hold, `account_id -> data-URL bytes` (for seeding the UI).
    pub fn peer_avatars(&self) -> std::collections::HashMap<String, Vec<u8>> {
        self.node.peer_avatars()
    }

    pub fn peer_profile_states(&self) -> std::collections::HashMap<String, Option<Vec<u8>>> {
        self.node.peer_profile_states()
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
        self.node
            .persist_account_adoption(&account.account_id(), || {
                crate::identity::account_keystore::save(
                    &self.account_path,
                    &self.password,
                    &account,
                )
                .map_err(|e| NodeError::Channel(format!("persist new account: {e}")))
            })?;
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
        let peer = {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            roster
                .get(peer_user_id)
                .cloned()
                .ok_or_else(|| NodeError::UnknownPeer(peer_user_id.to_string()))?
        };
        let linked = self
            .node
            .link_to_device_if(peer.addr, &peer.public, code, &mut authorize)
            .await?;
        authorize(&mut || {
            let account = crate::identity::account::Account::from_secret_bytes(linked.secret);
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
            self.node
                .persist_account_adoption(&account.account_id(), || {
                    crate::identity::account_keystore::save(
                        &self.account_path,
                        &self.password,
                        &account,
                    )
                    .map_err(|e| NodeError::Channel(format!("persist linked account: {e}")))
                })?;
            Ok(())
        })?;
        Ok(linked.account_id)
    }

    /// Create a channel; returns its conversation id.
    pub async fn create_channel(
        &self,
        name: &str,
        members: Vec<PublicIdentity>,
    ) -> Result<crate::eventlog::event::ConversationId, NodeError> {
        self.node.create_channel(name, members).await
    }

    /// Channels this node is a member of.
    pub fn list_channels(&self) -> Vec<crate::node::ChannelSummary> {
        self.node.list_channels()
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
    ) -> Vec<crate::node::ChannelActivity> {
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
    ) -> Vec<HistoryEntry> {
        self.node.channel_history(channel, limit)
    }

    /// Search all DMs + channels for `query`.
    pub fn search(&self, query: &str) -> Vec<crate::node::SearchHit> {
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
    pub fn reactions_dm(&self, peer: &PublicIdentity) -> Vec<crate::node::reaction::ReactionView> {
        self.node.reactions_dm(peer)
    }

    /// Aggregated reactions in a channel.
    pub fn channel_reactions(
        &self,
        channel: crate::eventlog::event::ConversationId,
    ) -> Vec<crate::node::reaction::ReactionView> {
        self.node.reactions(channel)
    }

    /// Aggregated reactions in the account conversation with `peer_account_id`.
    pub fn account_reactions(
        &self,
        peer_account_id: &str,
    ) -> Vec<crate::node::reaction::ReactionView> {
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
