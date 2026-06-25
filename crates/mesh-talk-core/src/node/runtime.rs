//! The desktop App context for the node: opens a per-user `Node`, starts
//! its network loops (discovery, accept, drain) and an inbound-DM forwarder, and
//! holds the handles. Tauri-agnostic — inbound DMs are delivered via an injected
//! `on_dm` callback, so this is unit-testable without a GUI.

use crate::discovery::service::{
    shared_announce, spawn_discovery_with_trigger, swap_announce, SharedAnnounce,
};
use crate::discovery::{Announce, PeerRecord, Roster};
use crate::eventlog::LogError;
use crate::identity::account_keystore;
use crate::identity::device::PublicIdentity;
use crate::identity::keystore;
use crate::node::name_directory::NameDirectory;
use crate::node::{HistoryEntry, Node, NodeError, ReceivedDm};
use crate::transport::net::discovery_socket;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;

/// How often the desktop node drains held DMs from the elected post office.
const DRAIN_INTERVAL_SECS: u64 = 3;

/// How often the node tries to pull chunks for files it has a manifest for but hasn't
/// fully received yet — directly from peers (covers PO-less LANs where the sender's
/// one-shot push is the only other delivery). A no-op when nothing is pending.
const FILE_PULL_INTERVAL_SECS: u64 = 3;

/// How often the node checks for freshly-discovered peers and re-publishes its current
/// avatar to them (so a new contact sees the photo without waiting for the next change).
/// A no-op when no new peers appeared and when we've never set an avatar.
const PROFILE_REPUBLISH_INTERVAL_SECS: u64 = 3;

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

/// A running node plus the background tasks that drive it. Dropping it
/// aborts every task (clean logout/shutdown).
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
    /// The encoded announce the discovery loops broadcast, shared so a display-name
    /// change can re-sign and swap it in live (no discovery restart).
    announce: SharedAnnounce,
    /// The OS-assigned TCP port the accept loop listens on (also needed to re-sign the
    /// announce on a rename).
    tcp_port: u16,
    /// Where the account keystore lives, so a successful device link can persist the
    /// adopted account secret (effective on next login).
    account_path: std::path::PathBuf,
    /// The session password, to re-encrypt the keystore when adopting a linked account.
    password: String,
    /// Notify shared with the discovery broadcast + scan loops; firing it forces an
    /// immediate re-announce and a /24 sweep (the manual "announce now / rescan").
    discovery_trigger: Arc<Notify>,
    tasks: Vec<JoinHandle<()>>,
}

impl NodeRuntime {
    /// Open the per-account node under `base_dir/accounts/<account_id>/` (keystore +
    /// durable logs encrypted with `password`), advertise `display_name`, and start
    /// discovery + the accept loop + a periodic post-office drain + an inbound
    /// forwarder that calls `on_dm` for each received DM.
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
    ) -> Result<NodeRuntime, RuntimeError> {
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
        let listener = crate::transport::net::bind_dual_stack_listener(0u16).await?;
        let tcp_port = listener.local_addr()?.port();

        // Build the announce BEFORE the identity is moved into the node, and wrap it in a
        // shared cell so a runtime rename can swap in a freshly re-signed announce.
        let announce =
            Announce::new_with_account(&identity, &account, display_name.to_string(), tcp_port);
        let announce = shared_announce(&announce);

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
            tokio::task::spawn_blocking(move || NameDirectory::open(&names_path, &password))
        };
        let node = {
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
            .map_err(|e| RuntimeError::Io(std::io::Error::other(format!("join error: {e}"))))??
        };
        let names = Arc::new(Mutex::new(names_handle.await.map_err(|e| {
            RuntimeError::Io(std::io::Error::other(format!("join error: {e}")))
        })??));

        let socket = Arc::new(discovery_socket(discovery_port)?);

        let discovery_trigger = Arc::new(Notify::new());
        let mut tasks: Vec<JoinHandle<()>> = Vec::new();
        tasks.extend(spawn_discovery_with_trigger(
            Arc::clone(&socket),
            Arc::clone(&roster),
            Arc::clone(&announce),
            self_uid.clone(),
            discovery_port,
            Some(Arc::clone(&discovery_trigger)),
        ));
        tasks.push(tokio::spawn(Arc::clone(&node).run_accept_loop(listener)));
        {
            let node = Arc::clone(&node);
            tasks.push(tokio::spawn(async move {
                loop {
                    node.drain_from_post_office().await;
                    tokio::time::sleep(Duration::from_secs(DRAIN_INTERVAL_SECS)).await;
                }
            }));
        }
        {
            let node = Arc::clone(&node);
            tasks.push(tokio::spawn(async move {
                loop {
                    node.pull_pending_files().await;
                    tokio::time::sleep(Duration::from_secs(FILE_PULL_INTERVAL_SECS)).await;
                }
            }));
        }
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
        // Re-publish our avatar to freshly-discovered peers: poll the roster, and for any
        // peer user-id we hadn't seen before, send our current profile (a no-op if we've
        // set no avatar). Bounded — `seen` only tracks ids, and re-publish skips known peers.
        {
            let node = Arc::clone(&node);
            let roster = Arc::clone(&roster);
            let names = Arc::clone(&names);
            tasks.push(tokio::spawn(async move {
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
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
                    // Re-publish our avatar to peers we hadn't seen before (a no-op if we've
                    // set no avatar). `seen` only tracks ids, so this stays bounded.
                    let newcomers = peers
                        .into_iter()
                        .filter(|p| !p.post_office && seen.insert(p.public.user_id()));
                    for peer in newcomers {
                        node.republish_profile_to(&peer).await;
                    }
                    tokio::time::sleep(Duration::from_secs(PROFILE_REPUBLISH_INTERVAL_SECS)).await;
                }
            }));
        }

        Ok(NodeRuntime {
            node,
            roster,
            names,
            user_id: self_uid,
            account_id,
            display_name: display_name.to_string(),
            announce,
            tcp_port,
            account_path: dir.join("account.keystore"),
            password: password.to_string(),
            discovery_trigger,
            tasks,
        })
    }

    /// This node's own user-id fingerprint.
    pub fn user_id(&self) -> &str {
        &self.user_id
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
        let announce = self.node.signed_announce(display_name, self.tcp_port);
        swap_announce(&self.announce, &announce);
        self.display_name = display_name.to_string();
        // Keep our own name in the durable directory too (it otherwise only tracks peers),
        // so `display_name_for(own_id)` stays consistent. A no-op when unchanged.
        let _ = self
            .names
            .lock()
            .expect("names mutex not poisoned")
            .record(&self.user_id, display_name);
        self.discovery_trigger.notify_waiters();
    }

    /// The local TCP port the node's accept loop is listening on.
    pub fn listen_tcp_port(&self) -> u16 {
        self.tcp_port
    }

    /// This node's cryptographic account id (stable across devices once linking
    /// is wired; today each device generates its own account on first run).
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    /// Force an immediate re-announce + /24 rescan, on top of the periodic timers.
    /// Useful when LAN discovery is flaky (e.g. first-contact lost to UDP drops):
    /// the user can press "announce now" to converge without waiting for the next
    /// tick. Idempotent and harmless — peers self-filter duplicate announces.
    pub fn trigger_discovery(&self) {
        self.discovery_trigger.notify_waiters();
    }

    /// A snapshot of currently-known peers.
    pub fn peers(&self) -> Vec<PeerRecord> {
        self.roster
            .lock()
            .expect("roster mutex not poisoned")
            .peers()
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
        self.roster
            .lock()
            .expect("roster mutex not poisoned")
            .get(user_id)
            .map(|p| p.public.clone())
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
        crate::identity::account_keystore::save(&self.account_path, &self.password, &account)
            .map_err(|e| NodeError::Channel(format!("persist new account: {e}")))?;
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
        let peer = {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            roster
                .get(peer_user_id)
                .cloned()
                .ok_or_else(|| NodeError::UnknownPeer(peer_user_id.to_string()))?
        };
        let linked = self
            .node
            .link_to_device(peer.addr, &peer.public, code)
            .await?;
        let account = crate::identity::account::Account::from_secret_bytes(linked.secret);
        crate::identity::account_keystore::save(&self.account_path, &self.password, &account)
            .map_err(|e| NodeError::Channel(format!("persist linked account: {e}")))?;
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

    /// The owner (creator) of a channel — the only principal allowed to change membership.
    pub fn channel_owner(&self, channel: crate::eventlog::event::ConversationId) -> String {
        self.node.channel_owner(channel)
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

impl Drop for NodeRuntime {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn start_opens_a_node_and_exposes_its_identity() {
        let dir = tempfile::tempdir().unwrap();
        // An uncommon discovery port so the test doesn't touch the real one.
        let dp = 47990;
        let runtime = NodeRuntime::start(
            dir.path(),
            "alice-user-id",
            "Alice",
            "pw",
            dp,
            |_dm| {},
            |_ch| {},
            |_f| {},
            |_p| {},
        )
        .await
        .expect("runtime starts");

        // The node opened and exposes a stable fingerprint; no peers yet.
        assert_eq!(runtime.user_id().len(), 32); // user_id is 32 hex chars
        assert!(runtime.peers().is_empty());
        assert!(runtime.peer_public("nobody").is_none());

        // Per-user files were created on disk under accounts/<user_id>/.
        let node_dir = dir.path().join("accounts").join("alice-user-id");
        assert!(node_dir.join("identity.keystore").exists());
        assert!(node_dir.join("messages.log").exists());
        assert!(node_dir.join("sent.log").exists());

        // Reopening the same dir reloads the SAME identity (persistent).
        let runtime2 = NodeRuntime::start(
            dir.path(),
            "alice-user-id",
            "Alice",
            "pw",
            dp,
            |_dm| {},
            |_ch| {},
            |_f| {},
            |_p| {},
        )
        .await
        .expect("runtime reopens");
        assert_eq!(runtime.user_id(), runtime2.user_id());

        // The cryptographic account is created on disk and stable across reopen.
        assert_eq!(runtime.account_id().len(), 32); // account_id is 32 hex chars
        assert!(node_dir.join("account.keystore").exists());
        assert_eq!(runtime.account_id(), runtime2.account_id());
    }

    #[tokio::test]
    async fn set_display_name_updates_advertised_name_and_own_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut runtime = NodeRuntime::start(
            dir.path(),
            "alice-user-id",
            "Alice",
            "pw",
            47991,
            |_dm| {},
            |_ch| {},
            |_f| {},
            |_p| {},
        )
        .await
        .expect("runtime starts");

        assert_eq!(runtime.display_name(), "Alice");
        let uid = runtime.user_id().to_string();

        // Rename to a permissive (spaces + emoji) display name.
        runtime.set_display_name("Alice Renamed 🐻");

        // The advertised name and our own durable record both reflect the change; the
        // identity (user_id/account_id) is untouched.
        assert_eq!(runtime.display_name(), "Alice Renamed 🐻");
        assert_eq!(
            runtime.display_name_for(&uid).as_deref(),
            Some("Alice Renamed 🐻")
        );
        assert_eq!(runtime.user_id(), uid);
    }
}
