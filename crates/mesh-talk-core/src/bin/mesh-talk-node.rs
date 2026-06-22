//! `mesh-talk-node`: a thin CLI over the `node` API — loads a
//! persistent identity, runs signed-announce LAN discovery and a TCP listener,
//! and drives 1:1 DMs, channels, and file transfers from a line-based REPL
//! (`/peers`, `/msg <prefix> <text>`, `/history <prefix>`,
//! `/channel-new <name> <member-prefix>`, `/channel-msg <chan-id-prefix> <text>`,
//! `/sendfile <peer-prefix> <path>`, `/quit`), printing inbound DMs, channel
//! messages, and files as they arrive.

use clap::Parser;
use mesh_talk_core::discovery::{Announce, PeerRecord, Roster, UserId};
use mesh_talk_core::identity::device::DeviceIdentity;
use mesh_talk_core::node::run_relay_accept_loop;
use mesh_talk_core::node::{
    discovery_socket, spawn_discovery, Node, ReceivedDm, DEFAULT_DISCOVERY_PORT,
};
use mesh_talk_core::postoffice::PostOffice;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

/// How often a normal node drains held DMs from the elected post office.
const DRAIN_INTERVAL_SECS: u64 = 3;

#[derive(Parser, Debug)]
#[command(
    name = "mesh-talk-node",
    about = "Mesh-Talk node (serverless E2E LAN DM)"
)]
struct Args {
    /// Path to the encrypted identity keystore (created if absent).
    #[arg(long)]
    keystore: String,
    /// Password that encrypts the keystore.
    #[arg(long)]
    password: String,
    /// Display name advertised to peers.
    #[arg(long)]
    name: String,
    /// TCP port to listen on for peer connections (0 = OS-assigned).
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// UDP port for signed-announce discovery (must match across peers).
    #[arg(long, default_value_t = DEFAULT_DISCOVERY_PORT)]
    discovery_port: u16,
    /// Run as a post office: a durable store-and-forward relay that advertises
    /// the post-office role (no DM REPL — it only relays).
    #[arg(long)]
    post_office: bool,
}

/// Why a `/msg` prefix did not resolve to exactly one peer.
#[derive(Debug, PartialEq, Eq)]
enum ResolveError {
    NotFound,
    Ambiguous(usize),
}

/// Resolve a `/msg` recipient by a `user_id` PREFIX against a roster snapshot.
/// Returns the unique matching peer's full `user_id`, or an error if zero or
/// more than one peer matches.
fn resolve_recipient(peers: &[PeerRecord], prefix: &str) -> Result<UserId, ResolveError> {
    let matches: Vec<UserId> = peers
        .iter()
        .map(|p| p.public.user_id())
        .filter(|uid| uid.starts_with(prefix))
        .collect();
    match matches.len() {
        0 => Err(ResolveError::NotFound),
        1 => Ok(matches.into_iter().next().expect("len checked == 1")),
        n => Err(ResolveError::Ambiguous(n)),
    }
}

/// Write one line to stdout and FLUSH it. Piped stdout is block-buffered, so a
/// bare `println!` would not reach a parent process promptly — every
/// rig-observable line must go through here.
fn emit(line: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// An advisory lock on a node's data directory: an `O_EXCL` lockfile created on startup
/// and removed on exit. The GUI is protected from two instances by single_instance; the
/// CLI is not, so two nodes sharing one keystore dir would both append to one
/// messages.log/sent.log and clobber records. This makes a second node sharing the dir
/// fail fast with a clear message instead of silently corrupting the logs.
struct DataDirLock {
    path: std::path::PathBuf,
}

impl DataDirLock {
    /// Acquire the lock for `data_dir`, failing if another LIVE node already holds it.
    /// The lockfile records the holder's PID; a lock left by a crashed/killed process
    /// (whose Drop never ran) is detected as stale and reclaimed, so a restart of the
    /// same node isn't blocked by its own previous instance.
    fn acquire(data_dir: &std::path::Path) -> std::io::Result<DataDirLock> {
        use std::io::Write;
        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join("node.lock");
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    let _ = write!(f, "{}", std::process::id());
                    return Ok(DataDirLock { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Someone holds (or held) the lock. If the recorded PID is no longer
                    // alive, the holder crashed/was killed before cleaning up — reclaim
                    // the stale lock and retry. Otherwise a live node owns it: fail fast.
                    let holder = std::fs::read_to_string(&path)
                        .ok()
                        .and_then(|s| s.trim().parse::<u32>().ok());
                    if holder.is_none_or(|pid| !pid_is_alive(pid)) {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!(
                            "data dir {} is locked by another mesh-talk-node (pid {}); \
                             remove {} if no node is running",
                            data_dir.display(),
                            holder.expect("checked Some above"),
                            path.display()
                        ),
                    ));
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// Whether a process with `pid` is currently alive (signal 0 probe). Used only to decide
/// if a leftover lockfile is stale. On the rare PID-reuse race this can over-report
/// "alive" and refuse the lock — a safe (conservative) failure, never silent corruption.
#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    // SAFETY: `kill` with signal 0 performs no signal delivery — it only checks whether the
    // process exists / is permitted to be signalled. No memory is touched.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true; // exists and signallable
    }
    // Only ESRCH (no such process) proves it's gone; EPERM means it exists but isn't ours.
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Non-unix fallback: assume any recorded holder is alive (no cheap PID probe), so the
/// lock is strict. A leftover lockfile must then be removed by hand — surfaced in the
/// error message.
#[cfg(not(unix))]
fn pid_is_alive(_pid: u32) -> bool {
    true
}

impl Drop for DataDirLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Fail fast if another CLI node is already using this keystore's data dir (the GUI is
    // guarded by single_instance; the CLI isn't). Held for the process lifetime via Drop.
    let data_dir = std::path::Path::new(&args.keystore)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();
    let _data_dir_lock = DataDirLock::acquire(&data_dir)?;

    if args.post_office {
        return run_post_office(args).await;
    }

    // Persistent identity (created on first run).
    let identity = mesh_talk_core::identity::keystore::load_or_create(
        std::path::Path::new(&args.keystore),
        &args.password,
    )?;
    let user_id = identity.public().user_id();

    // Persistent cross-device account key (sibling of the device keystore).
    let account_path = std::path::Path::new(&args.keystore)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("account.keystore");
    let account =
        mesh_talk_core::identity::account_keystore::load_or_create(&account_path, &args.password)?;

    // TCP listener for peer connections; dual-stack (IPv6 + IPv4-mapped, falling back to
    // IPv4) so the node is reachable over IPv6/link-local too. Resolve the actual port.
    let listener = mesh_talk_core::node::bind_dual_stack_listener(args.port).await?;
    let tcp_port = listener.local_addr()?.port();

    // Build the signed announce (borrows identity) BEFORE moving identity into the node.
    let announce = Announce::new_with_account(&identity, &account, args.name.clone(), tcp_port);

    // Shared roster (discovery writes it; the node + REPL read it) and the node.
    let roster: Arc<Mutex<Roster>> = Arc::new(Mutex::new(Roster::default()));
    let (incoming_tx, mut incoming_rx) = mpsc::unbounded_channel::<ReceivedDm>();
    let (channel_tx, mut channel_rx) =
        mpsc::unbounded_channel::<mesh_talk_core::node::ReceivedChannelMessage>();
    let (file_tx, mut file_rx) = mpsc::unbounded_channel::<mesh_talk_core::node::ReceivedFile>();
    // Derive log paths from the keystore path (sibling files, same directory).
    let keystore_path = std::path::Path::new(&args.keystore);
    let data_dir = keystore_path.parent().unwrap_or(std::path::Path::new("."));
    let log_path = data_dir.join("messages.log");
    let sent_path = data_dir.join("sent.log");
    let node = Node::open_with_account(
        identity,
        account,
        Arc::clone(&roster),
        incoming_tx,
        channel_tx,
        file_tx,
        &log_path,
        &sent_path,
        &args.password,
    )?;

    // Discovery: one shared reuse+multicast socket drives all loops (listen with
    // announce/response, broadcast, /24 scan fallback, periodic re-join).
    let socket = Arc::new(discovery_socket(args.discovery_port)?);
    spawn_discovery(
        Arc::clone(&socket),
        Arc::clone(&roster),
        announce,
        user_id.clone(),
        args.discovery_port,
    );

    // Inbound: serve sync rounds on each accepted connection.
    tokio::spawn(Arc::clone(&node).run_accept_loop(listener));

    // Print received DMs as they arrive.
    tokio::spawn(async move {
        while let Some(dm) = incoming_rx.recv().await {
            // DMs are text-only in this CLI; lossy decode is safe here.
            let text = String::from_utf8_lossy(&dm.text);
            emit(&format!("from {} ({}): {}", dm.from, dm.from_name, text));
        }
    });

    // Print inbound channel messages as they arrive.
    tokio::spawn(async move {
        while let Some(m) = channel_rx.recv().await {
            // Channel messages are text-only in this CLI; lossy decode is safe here.
            let text = String::from_utf8_lossy(&m.text);
            emit(&format!(
                "channel {} from {}: {}",
                hex::encode(m.channel_id.as_bytes()),
                m.from,
                text
            ));
        }
    });

    // Print inbound files as they arrive, saving them into the node's data dir.
    {
        let node = Arc::clone(&node);
        let data_dir = data_dir.to_path_buf();
        tokio::spawn(async move {
            while let Some(f) = file_rx.recv().await {
                // Save into a trusted dir; the manifest name is never trusted to escape it.
                match node.save_file_into_dir(f.file_conv, &data_dir) {
                    Ok(path) => emit(&format!(
                        "file from {}: {} ({} bytes) saved {}",
                        f.from,
                        f.name,
                        f.size,
                        path.display()
                    )),
                    Err(e) => emit(&format!(
                        "file from {}: {} ({} bytes) save failed: {e}",
                        f.from, f.name, f.size
                    )),
                }
            }
        });
    }

    // Periodically pull DMs held for us by the elected post office (delivered
    // while we were offline / unreachable). Runs once now, then every interval.
    {
        let node = Arc::clone(&node);
        tokio::spawn(async move {
            loop {
                node.drain_from_post_office().await;
                tokio::time::sleep(Duration::from_secs(DRAIN_INTERVAL_SECS)).await;
            }
        });
    }

    emit(&format!(
        "node {user_id} listening on tcp/{tcp_port}, discovery udp/{}",
        args.discovery_port
    ));

    // Line-based REPL on stdin.
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line == "/quit" {
            break;
        } else if line == "/peers" {
            let peers = roster.lock().expect("roster mutex not poisoned").peers();
            if peers.is_empty() {
                emit("(no peers yet)");
            } else {
                for p in peers {
                    emit(&format!(
                        "peer {} {} {}",
                        p.public.user_id(),
                        p.name,
                        p.addr
                    ));
                }
            }
        } else if let Some(rest) = line.strip_prefix("/msg ") {
            handle_msg(&node, &roster, rest);
        } else if let Some(rest) = line.strip_prefix("/history ") {
            handle_history(&node, &roster, rest);
        } else if line == "/history" {
            emit("usage: /history <user_id-prefix> [n]");
        } else if let Some(rest) = line.strip_prefix("/channel-new ") {
            handle_channel_new(&node, &roster, rest);
        } else if let Some(rest) = line.strip_prefix("/channel-msg ") {
            handle_channel_msg(&node, rest);
        } else if let Some(rest) = line.strip_prefix("/sendfile ") {
            handle_sendfile(&node, &roster, rest);
        } else if !line.is_empty() {
            emit("commands: /peers, /msg <user_id-prefix> <text>, /history <user_id-prefix> [n], /channel-new <name> <member-prefix>, /channel-msg <chan-id-prefix> <text>, /sendfile <peer-prefix> <path>, /quit");
        }
    }
    Ok(())
}

/// Run as a post office: load the identity, advertise the post-office role,
/// serve a durable `PostOffice` relay. No DM send/drain — a pure relay. Runs the
/// accept loop forever (stop with Ctrl-C / kill).
///
/// Note: cold start does TWO password-KDF unlocks back to back — the keystore
/// (`load_or_create`) and the relay log (`PostOffice::open`) — so the startup
/// line can take ~15s to appear on first run. Tooling that waits for a relay to
/// come up should poll for the startup line, not fixed-sleep.
async fn run_post_office(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let identity = mesh_talk_core::identity::keystore::load_or_create(
        std::path::Path::new(&args.keystore),
        &args.password,
    )?;
    let user_id = identity.public().user_id();

    // Persistent cross-device account key (sibling of the device keystore).
    let account_path = std::path::Path::new(&args.keystore)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("account.keystore");
    let account =
        mesh_talk_core::identity::account_keystore::load_or_create(&account_path, &args.password)?;

    let listener = mesh_talk_core::node::bind_dual_stack_listener(args.port).await?;
    let tcp_port = listener.local_addr()?.port();

    // The relay advertises the post-office role.
    let announce =
        Announce::new_post_office_with_account(&identity, &account, args.name.clone(), tcp_port);

    // Two identity copies: one is moved into the durable PostOffice, one is used
    // for the Noise handshake in the accept loop (same keypair, different owner).
    let (ed, x) = identity.secret_bytes();
    let transport_identity = DeviceIdentity::from_secret_bytes(ed, x);
    let relay_path = std::path::Path::new(&args.keystore)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("relay.log");
    let store = Arc::new(Mutex::new(PostOffice::open(
        &relay_path,
        &args.password,
        identity,
    )?));

    // Discovery: advertise ourselves (and harmlessly track peers) on the shared socket.
    let roster: Arc<Mutex<Roster>> = Arc::new(Mutex::new(Roster::default()));
    let socket = Arc::new(discovery_socket(args.discovery_port)?);
    spawn_discovery(
        Arc::clone(&socket),
        Arc::clone(&roster),
        announce,
        user_id.clone(),
        args.discovery_port,
    );

    emit(&format!(
        "post-office {user_id} listening on tcp/{tcp_port}, discovery udp/{}",
        args.discovery_port
    ));

    // Serve relay connections until killed. This loop never returns; the trailing
    // Ok(()) is unreachable in practice (the process is stopped by a signal).
    run_relay_accept_loop(transport_identity, listener, store).await;
    Ok(())
}

/// Parse and dispatch a `/msg <prefix> <text>` command: resolve the recipient
/// against the current roster, then send the DM on a spawned task so the REPL
/// stays responsive while the dial + sync round runs.
fn handle_msg(node: &Arc<Node>, roster: &Arc<Mutex<Roster>>, rest: &str) {
    let mut parts = rest.splitn(2, ' ');
    let prefix = parts.next().unwrap_or("").trim();
    let text = parts.next().unwrap_or("").to_string();
    if prefix.is_empty() || text.is_empty() {
        emit("usage: /msg <user_id-prefix> <text>");
        return;
    }
    let peers = roster.lock().expect("roster mutex not poisoned").peers();
    match resolve_recipient(&peers, prefix) {
        Ok(uid) => {
            let node = Arc::clone(node);
            tokio::spawn(async move {
                if let Err(e) = node.send_dm(&uid, text.as_bytes()).await {
                    emit(&format!("send failed: {e}"));
                }
            });
        }
        Err(ResolveError::NotFound) => emit(&format!("no peer matches prefix '{prefix}'")),
        Err(ResolveError::Ambiguous(n)) => {
            emit(&format!("'{prefix}' matches {n} peers; be more specific"))
        }
    }
}

/// Parse and dispatch `/history <peer-prefix> [n]`: resolve the peer, then print
/// the last n (default 20) messages of that DM, both directions, oldest first.
fn handle_history(node: &Arc<Node>, roster: &Arc<Mutex<Roster>>, rest: &str) {
    let mut parts = rest.split_whitespace();
    let prefix = parts.next().unwrap_or("");
    let limit: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(20);
    if prefix.is_empty() {
        emit("usage: /history <user_id-prefix> [n]");
        return;
    }
    // Resolve the peer under the roster lock, then DROP the lock before calling
    // into the node (which locks the roster again — std Mutex is not reentrant).
    let peer = {
        let roster = roster.lock().expect("roster mutex not poisoned");
        let peers = roster.peers();
        match resolve_recipient(&peers, prefix) {
            Ok(uid) => roster.get(&uid).cloned(),
            Err(ResolveError::NotFound) => {
                emit(&format!("no peer matches prefix '{prefix}'"));
                return;
            }
            Err(ResolveError::Ambiguous(n)) => {
                emit(&format!("'{prefix}' matches {n} peers; be more specific"));
                return;
            }
        }
    };
    let Some(peer) = peer else {
        emit("peer vanished from roster");
        return;
    };
    let entries = node.dm_history(&peer.public, limit);
    if entries.is_empty() {
        emit("(no history)");
        return;
    }
    for e in entries {
        let text = String::from_utf8_lossy(&e.text);
        emit(&format!("[{}] {}: {text}", e.wall_clock, e.who));
    }
}

/// Parse and dispatch `/channel-new <name> <member-prefix>`: resolve one peer to
/// include as a member, create the channel (the creator is added automatically),
/// then print the new channel id (hex). Runs on a spawned task (create distributes
/// sender keys to members over the network).
fn handle_channel_new(node: &Arc<Node>, roster: &Arc<Mutex<Roster>>, rest: &str) {
    let mut parts = rest.split_whitespace();
    let name = parts.next().unwrap_or("").to_string();
    let prefix = parts.next().unwrap_or("").to_string();
    if name.is_empty() || prefix.is_empty() {
        emit("usage: /channel-new <name> <member-prefix>");
        return;
    }
    // Resolve the member under the roster lock, then DROP it before the async call
    // (the node locks the roster again; std Mutex is not reentrant).
    let member = {
        let roster = roster.lock().expect("roster mutex not poisoned");
        let peers = roster.peers();
        match resolve_recipient(&peers, &prefix) {
            Ok(uid) => roster.get(&uid).map(|p| p.public.clone()),
            Err(ResolveError::NotFound) => {
                emit(&format!("no peer matches prefix '{prefix}'"));
                return;
            }
            Err(ResolveError::Ambiguous(n)) => {
                emit(&format!("'{prefix}' matches {n} peers; be more specific"));
                return;
            }
        }
    };
    let Some(member) = member else {
        emit("peer vanished from roster");
        return;
    };
    let node = Arc::clone(node);
    tokio::spawn(async move {
        match node.create_channel(&name, vec![member]).await {
            Ok(id) => emit(&format!("channel {} created", hex::encode(id.as_bytes()))),
            Err(e) => emit(&format!("channel create failed: {e}")),
        }
    });
}

/// Parse and dispatch `/channel-msg <chan-id-prefix> <text>`: resolve the channel by a
/// hex-id prefix against the node's known channels, then send the message on a spawned
/// task so the REPL stays responsive while distribution runs.
fn handle_channel_msg(node: &Arc<Node>, rest: &str) {
    let mut parts = rest.splitn(2, ' ');
    let prefix = parts.next().unwrap_or("").trim().to_string();
    let text = parts.next().unwrap_or("").to_string();
    if prefix.is_empty() || text.is_empty() {
        emit("usage: /channel-msg <chan-id-prefix> <text>");
        return;
    }
    let matches: Vec<_> = node
        .list_channels()
        .into_iter()
        .filter(|c| hex::encode(c.id.as_bytes()).starts_with(&prefix))
        .map(|c| c.id)
        .collect();
    let channel = match matches.len() {
        0 => {
            emit(&format!("no channel matches prefix '{prefix}'"));
            return;
        }
        1 => matches[0],
        n => {
            emit(&format!(
                "'{prefix}' matches {n} channels; be more specific"
            ));
            return;
        }
    };
    let node = Arc::clone(node);
    tokio::spawn(async move {
        if let Err(e) = node.send_channel_message(channel, text.as_bytes()).await {
            emit(&format!("channel send failed: {e}"));
        }
    });
}

/// Parse and dispatch `/sendfile <peer-prefix> <path>`: resolve the recipient and send
/// the file at `path` on a spawned task (streamed seal + dial + sync round).
fn handle_sendfile(node: &Arc<Node>, roster: &Arc<Mutex<Roster>>, rest: &str) {
    let mut parts = rest.splitn(2, ' ');
    let prefix = parts.next().unwrap_or("").trim().to_string();
    let path = parts.next().unwrap_or("").trim().to_string();
    if prefix.is_empty() || path.is_empty() {
        emit("usage: /sendfile <peer-prefix> <path>");
        return;
    }
    let peers = roster.lock().expect("roster mutex not poisoned").peers();
    match resolve_recipient(&peers, &prefix) {
        Ok(uid) => {
            let node = Arc::clone(node);
            tokio::spawn(async move {
                match node.send_file_dm(&uid, std::path::Path::new(&path)).await {
                    Ok(_) => emit(&format!("file sent to {uid}")),
                    Err(e) => emit(&format!("file send failed: {e}")),
                }
            });
        }
        Err(ResolveError::NotFound) => emit(&format!("no peer matches prefix '{prefix}'")),
        Err(ResolveError::Ambiguous(n)) => {
            emit(&format!("'{prefix}' matches {n} peers; be more specific"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_talk_core::identity::device::DeviceIdentity;
    use std::net::SocketAddr;
    use std::time::Instant;

    fn peer(name: &str) -> PeerRecord {
        PeerRecord {
            public: DeviceIdentity::generate().public(),
            addr: "127.0.0.1:4000".parse::<SocketAddr>().unwrap(),
            name: name.to_string(),
            post_office: false,
            account_id: None,
            last_seen: Instant::now(),
        }
    }

    #[test]
    fn unique_prefix_resolves_to_the_full_user_id() {
        let p = peer("Alice");
        let full = p.public.user_id();
        let prefix = &full[..8];
        let peers = vec![p];
        assert_eq!(resolve_recipient(&peers, prefix), Ok(full));
    }

    #[test]
    fn no_match_is_not_found() {
        let peers = vec![peer("Alice")];
        // user_ids are lowercase hex; "zzzz" can never be a prefix.
        assert_eq!(
            resolve_recipient(&peers, "zzzz"),
            Err(ResolveError::NotFound)
        );
    }

    #[test]
    fn full_user_id_as_prefix_resolves() {
        // Users often paste a complete user_id; an exact match must still resolve.
        let p = peer("Alice");
        let full = p.public.user_id();
        let peers = vec![p];
        assert_eq!(resolve_recipient(&peers, &full), Ok(full));
    }

    #[test]
    fn ambiguous_prefix_reports_the_count() {
        let peers = vec![peer("Alice"), peer("Bob"), peer("Carol")];
        // The empty prefix matches every peer (random fingerprints share no
        // deterministic non-empty prefix, so "" is the portable way to force a
        // multi-way match); the REPL guards against an empty prefix separately.
        assert_eq!(
            resolve_recipient(&peers, ""),
            Err(ResolveError::Ambiguous(3))
        );
    }

    #[test]
    fn empty_roster_is_not_found() {
        assert_eq!(resolve_recipient(&[], "ab"), Err(ResolveError::NotFound));
    }
}
