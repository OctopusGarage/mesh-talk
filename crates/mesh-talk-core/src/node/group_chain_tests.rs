//! S12: three simulated LANs (A-B, C-D, E-F), joined only by B-C and D-E.
//! Every edge is real authenticated loopback TCP; roster adjacency is the
//! simulated reachability boundary. No discovery task, relay, or all-to-all route
//! is installed. Explicit production recovery ticks make each boundary observable.
use super::Node;
use crate::{
    discovery::{Announce, Roster},
    eventlog::{ConversationId, EventLog},
    identity::device::DeviceIdentity,
};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle};

struct Chain {
    _dir: tempfile::TempDir,
    nodes: Vec<Arc<Node>>,
    addresses: Vec<SocketAddr>,
    servers: Vec<Option<JoinHandle<()>>>,
}

#[tokio::test]
async fn current_group_metadata_crosses_real_tcp_after_owner_goes_offline() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut chain = Chain::new().await;
        let group = chain.nodes[0]
            .create_channel_invited("metadata", vec![chain.nodes[1].identity.public()])
            .await
            .unwrap();
        chain.round(1, 0, group).await;
        chain.nodes[1].accept_channel_invite(group).await.unwrap();
        chain.round(1, 0, group).await;
        chain.nodes[0]
            .set_channel_history_policy(group, crate::channel::HistoryPolicy::Off)
            .await
            .unwrap();
        chain.nodes[0]
            .set_channel_announcement_if(group, Some("Current announcement"), None, |commit| {
                commit()
            })
            .await
            .unwrap();
        chain.nodes[0]
            .add_channel_member(group, chain.nodes[2].identity.public())
            .await
            .unwrap();
        chain.round(1, 0, group).await;
        chain.round(2, 1, group).await;
        let mut denied = false;
        for _ in 0..128 {
            match chain.nodes[2].channel_metadata(group) {
                Err(_) => {
                    denied = true;
                    break;
                }
                Ok(value) => {
                    assert!(
                        value.query_pending,
                        "unaccepted membership must not become readable"
                    );
                    assert!(value.cover.is_none());
                    assert!(value.announcement.is_none());
                    assert!(value.announcement_history.is_empty());
                }
            }
            tokio::task::yield_now().await;
        }
        assert!(denied, "prepared metadata must deny unaccepted membership");
        chain.nodes[2].accept_channel_invite(group).await.unwrap();
        chain.round(1, 2, group).await;
        chain.servers[0].take().unwrap().abort();
        assert!(
            chain.nodes[2]
                .channel_metadata(group)
                .unwrap()
                .announcement
                .is_none()
        );
        let mut released = Vec::new();
        for _ in 0..128 {
            released = chain.nodes[1].retry_group_metadata().unwrap();
            if !released.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(released, vec![group]);
        chain.round(2, 1, group).await;
        let mut metadata = None;
        for _ in 0..128 {
            let value = chain.nodes[2].channel_metadata(group).unwrap();
            if !value.query_pending {
                metadata = Some(value);
                break;
            }
            assert!(value.announcement.is_none());
            tokio::task::yield_now().await;
        }
        assert_eq!(
            metadata
                .expect("selected metadata must finish")
                .announcement
                .unwrap()
                .content
                .as_deref(),
            Some("Current announcement")
        );
        assert!(
            chain.nodes[2]
                .channel_history(group, 100)
                .unwrap()
                .is_empty()
        );
    })
    .await
    .expect("group metadata must converge over a reachable accepted holder");
}

impl Drop for Chain {
    fn drop(&mut self) {
        for server in self.servers.iter().flatten() {
            server.abort();
        }
    }
}

impl Chain {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut nodes = Vec::new();
        let mut addresses = Vec::new();
        let mut listeners = Vec::new();
        for i in 0..7 {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            addresses.push(listener.local_addr().unwrap());
            listeners.push(listener);
            let (dm, _) = mpsc::unbounded_channel();
            let (channel, _) = mpsc::unbounded_channel();
            let (file, _) = mpsc::unbounded_channel();
            // Sidecars and managed media belong to the directory, not the log
            // filename. Independent network peers must never share that store.
            let node_dir = dir.path().join(i.to_string());
            std::fs::create_dir(&node_dir).unwrap();
            let node = Node::open(
                DeviceIdentity::generate(),
                Arc::new(Mutex::new(Roster::default())),
                dm,
                channel,
                file,
                &node_dir.join("events.log"),
                &node_dir.join("sent.log"),
                "pw",
            )
            .unwrap();
            node.set_post_office_enabled(false);
            nodes.push(node);
        }
        for i in 0..5 {
            for (from, to) in [(i, i + 1), (i + 1, i)] {
                nodes[from].roster.lock().unwrap().update(
                    &Announce::new(&nodes[to].identity, "adjacent member", addresses[to].port()),
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    &nodes[from].user_id(),
                );
            }
        }
        let servers = listeners
            .into_iter()
            .zip(&nodes)
            .map(|(listener, node)| Some(Self::serve(Arc::clone(node), listener)))
            .collect();
        Self {
            _dir: dir,
            nodes,
            addresses,
            servers,
        }
    }

    fn serve(node: Arc<Node>, listener: TcpListener) -> JoinHandle<()> {
        // Use the production admission/serving path without background recovery:
        // each test drives precisely the same recovery operation used by the host.
        tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let node = Arc::clone(&node);
                connections.spawn(async move {
                    if let Ok(channel) = node.privacy_accept(stream).await {
                        node.serve_connection(channel).await;
                    }
                });
                while connections.try_join_next().is_some() {}
            }
        })
    }

    async fn round(&self, from: usize, to: usize, channel: ConversationId) {
        assert!(from.abs_diff(to) == 1, "only adjacent members have routes");
        let mut connection = self.nodes[from]
            .privacy_dial(self.addresses[to], &self.nodes[to].identity.public())
            .await
            .unwrap();
        let store = self.nodes[from].sync_store(&self.nodes[to].identity.public());
        super::session::request_round(&mut connection, &store, channel)
            .await
            .unwrap();
        self.nodes[from].process_channel(channel);
        // The server processes the final appended round after its response.
        tokio::task::yield_now().await;
        self.nodes[to].process_channel(channel);
    }

    // No background recovery is running in this topology fixture. Advance only
    // already-retained local work, never create an extra route across a partition.
    async fn settle_retained(&self, index: usize, channel: ConversationId) {
        let node = &self.nodes[index];
        tokio::time::timeout(Duration::from_secs(5), async {
            node.await_channel_metadata(channel).await.unwrap();
            loop {
                match node.process_channel_reception_inner(channel, "live", false) {
                    Ok(()) | Err(super::NodeError::Log(crate::eventlog::LogError::StaleCursor)) => {
                    }
                    Err(error) => panic!("retained channel work failed: {error}"),
                }
                node.process_channel_file_reception(channel, "live")
                    .unwrap();
                let pending: bool = node
                    .storage
                    .database()
                    .read(|c| {
                        c.query_row(
                            "SELECT EXISTS(SELECT 1 FROM channel_runtime_work WHERE channel=?1)",
                            [channel.as_bytes().as_slice()],
                            |row| row.get(0),
                        )
                        .map_err(Into::into)
                    })
                    .unwrap();
                if !pending && node.pending_channel_file().is_none() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("bounded retained channel work must converge locally");
    }

    async fn accepted_group(&self) -> ConversationId {
        let channel = self.nodes[0]
            .create_channel_invited(
                "three LANs",
                self.nodes[1..6]
                    .iter()
                    .map(|node| node.identity.public())
                    .collect(),
            )
            .await
            .unwrap();
        for i in 1..6 {
            self.round(i, i - 1, channel).await;
            assert!(
                self.nodes[i]
                    .list_channels()
                    .unwrap()
                    .iter()
                    .any(|c| c.id == channel && c.pending_invite)
            );
            self.nodes[i].accept_channel_invite(channel).await.unwrap();
        }
        // Bring every signed acceptance back to A, then distribute that causal
        // ancestry forward before A encrypts the tested message.
        for i in (0..5).rev() {
            self.round(i, i + 1, channel).await;
        }
        for i in 1..6 {
            self.round(i, i - 1, channel).await;
        }
        for node in &self.nodes[..6] {
            assert!(
                node.list_channels()
                    .unwrap()
                    .iter()
                    .any(|c| c.id == channel && !c.pending_invite)
            );
        }
        assert_eq!(self.nodes[0].roster.lock().unwrap().peers().len(), 1);
        let roster = self.nodes[0].roster.lock().unwrap();
        let remote_c = self.nodes[2].user_id();
        assert!(
            !roster
                .peers()
                .iter()
                .any(|peer| peer.public.user_id() == remote_c
                    && !peer.presence_evidence(std::time::Instant::now()).is_empty()),
            "accepted group ancestry through B cannot establish C's presence at A"
        );
        channel
    }

    async fn recover_retained(&self, index: usize, channel: ConversationId) {
        // The original recovery deadline includes deferred local processing.
        // No worker or additional network route is introduced by this barrier.
        tokio::time::timeout(Duration::from_secs(5), async {
            self.nodes[index].pull_one_channel_from_member(0).await;
            self.settle_retained(index, channel).await;
        })
        .await
        .expect("bounded adjacent recovery and retained projection");
    }

    fn has_text(&self, index: usize, channel: ConversationId, text: &[u8]) -> bool {
        self.nodes[index]
            .channel_history(channel, 100)
            .unwrap()
            .iter()
            .any(|entry| entry.text == text)
    }
}

#[tokio::test]
async fn group_chain_crosses_three_lans_without_sender_routes_or_relays() {
    let chain = Chain::new().await;
    let group = chain.accepted_group().await;
    chain.nodes[0]
        .send_channel_message(group, b"A to F through members")
        .await
        .unwrap();
    for i in 1..6 {
        chain.recover_retained(i, group).await;
        assert!(
            chain.has_text(i, group, b"A to F through members"),
            "member {i} decrypts original author's event"
        );
    }
    let original = chain.nodes[0].channel_history(group, 100).unwrap()[0].id;
    for node in &chain.nodes[1..6] {
        assert_eq!(node.channel_history(group, 100).unwrap()[0].id, original);
    }
    // A fully authenticated nonmember guesses the group id and bypasses the
    // local projection using a raw sync store: the serving member must deny it.
    let outsider = &chain.nodes[6];
    let mut connection = outsider
        .privacy_dial(chain.addresses[4], &chain.nodes[4].identity.public())
        .await
        .unwrap();
    let unguarded = Mutex::new(EventLog::default());
    let _ = super::session::request_round(&mut connection, &unguarded, group).await;
    assert!(
        unguarded.lock().unwrap().events(&group).is_empty(),
        "no group metadata, keys, or content disclosed to a nonmember"
    );
    // A second reconciliation does not duplicate the delivered message.
    chain.recover_retained(5, group).await;
    assert_eq!(chain.nodes[5].channel_history(group, 100).unwrap().len(), 1);
    // Removal rotates the epoch. Even a formerly accepted member cannot
    // recover the later sender key or ciphertext by guessing the group id.
    chain.nodes[0]
        .remove_channel_member(group, &chain.nodes[5].user_id())
        .await
        .unwrap();
    for i in 1..5 {
        chain.recover_retained(i, group).await;
    }
    chain.nodes[0]
        .send_channel_message(group, b"after F removal")
        .await
        .unwrap();
    for i in 1..5 {
        chain.recover_retained(i, group).await;
    }
    assert!(chain.has_text(4, group, b"after F removal"));
    let mut removed_connection = chain.nodes[5]
        .privacy_dial(chain.addresses[4], &chain.nodes[4].identity.public())
        .await
        .unwrap();
    let removed_store = Mutex::new(EventLog::default());
    let _ = super::session::request_round(&mut removed_connection, &removed_store, group).await;
    assert!(
        removed_store.lock().unwrap().events(&group).is_empty(),
        "removed member cannot fetch rekey or later ciphertext"
    );
    assert!(!chain.has_text(5, group, b"after F removal"));
}

#[tokio::test]
async fn metered_group_file_replica_pauses_both_directions_without_losing_history_or_chunks() {
    assert_metered_group_file_replica(crate::file::FileKind::Media).await;
}

#[tokio::test]
async fn metered_group_file_replica_preserves_explicit_attachment_consent() {
    assert_metered_group_file_replica(crate::file::FileKind::File).await;
}

async fn assert_metered_group_file_replica(kind: crate::file::FileKind) {
    use super::NetworkCost;
    use crate::eventlog::sync::SyncStore;
    let chain = Chain::new().await;
    let group = chain.accepted_group().await;
    let source = chain._dir.path().join("metered.png");
    // C has authenticated A previously, but has no current route to A. File
    // consent validates the original author independently of recovery holder B.
    {
        let mut roster = chain.nodes[2].roster.lock().unwrap();
        roster.update(
            &Announce::new(&chain.nodes[0].identity, "original author", 9),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &chain.nodes[2].user_id(),
        );
        roster.forget(&chain.nodes[0].identity.public());
    }
    let mut payload = vec![7; crate::file::CHUNK_SIZE * 3];
    payload[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    std::fs::write(&source, &payload).unwrap();
    let file = chain.nodes[0]
        .send_file_channel(group, &source, kind)
        .await
        .unwrap();
    chain.round(1, 0, group).await;
    chain.nodes[1].process_file_events(group);
    if kind == crate::file::FileKind::File {
        assert_eq!(chain.nodes[1].file_progress(file).unwrap().unwrap().done, 0);
        chain.nodes[1].decide_attachment(file, true).unwrap();
    }
    chain.round(1, 0, file).await;
    let holder = &chain.nodes[1];
    assert!(holder.file_progress(file).unwrap().unwrap().done > 0);
    let retained = holder.log.lock().unwrap().events(&file).unwrap().len();
    let chunk_ids: std::collections::HashSet<_> = holder
        .log
        .lock()
        .unwrap()
        .events(&file)
        .unwrap()
        .iter()
        .map(|event| event.id)
        .collect();
    let group_events = holder.log.lock().unwrap().events(&group).unwrap().len();
    let peer = chain.nodes[2].identity.public();
    let inbound = holder.serve_sync_store(&peer);
    let outbound = holder.sync_store(&peer);
    assert!(!inbound.lock().unwrap().event_ids(&file).unwrap().is_empty());

    holder.set_network_cost(NetworkCost::Restricted);
    for store in [&inbound, &outbound] {
        let store = store.lock().unwrap();
        assert_eq!(
            store
                .event_ids(&file)
                .unwrap()
                .into_iter()
                .collect::<std::collections::HashSet<_>>(),
            chunk_ids,
            "a paused holder must advertise retained chunks to avoid duplicate downloads"
        );
        assert!(
            store
                .events_excluding(&file, &Default::default())
                .unwrap()
                .is_empty()
        );
        assert!(store.authorized_event_ids(&file).unwrap().is_empty());
        assert!(
            store
                .authorized_event_page(&file, &chunk_ids, 50_000)
                .unwrap()
                .is_empty()
        );
        assert!(
            !store.event_ids(&group).unwrap().is_empty(),
            "text and offers remain available"
        );
    }
    chain.round(2, 1, group).await;
    chain.settle_retained(2, group).await;
    if kind == crate::file::FileKind::File {
        chain.nodes[2].decide_attachment(file, true).unwrap();
    }
    chain.round(2, 1, file).await;
    assert!(
        chain.nodes[2]
            .log
            .lock()
            .unwrap()
            .events(&file)
            .unwrap()
            .is_empty()
    );
    assert_eq!(chain.nodes[2].file_progress(file).unwrap().unwrap().done, 0);
    assert!(!chain.nodes[2].file_ready_to_save(file).unwrap());
    assert_eq!(
        holder.log.lock().unwrap().events(&file).unwrap().len(),
        retained
    );
    assert_eq!(
        holder.log.lock().unwrap().events(&group).unwrap().len(),
        group_events
    );
    assert!(holder.channel_replica_enabled(group, &holder.user_id()));
    // The original source still supports the recipient's explicit download.
    chain.nodes[0].set_network_cost(NetworkCost::Restricted);
    assert!(
        !chain.nodes[0]
            .serve_sync_store(&holder.identity.public())
            .lock()
            .unwrap()
            .event_ids(&file)
            .unwrap()
            .is_empty()
    );

    holder.set_network_cost(NetworkCost::Unrestricted);
    chain.round(2, 1, file).await;
    assert_eq!(
        chain.nodes[2].file_progress(file).unwrap().unwrap().done,
        holder.file_progress(file).unwrap().unwrap().done
    );
    holder.set_network_cost(NetworkCost::Restricted);
    let mut connection = holder
        .privacy_dial(chain.addresses[2], &peer)
        .await
        .unwrap();
    let report = super::session::request_round(&mut connection, &outbound, file)
        .await
        .unwrap();
    assert_eq!(
        report.duplicates, 0,
        "a metered holder must not re-download its retained chunks"
    );
    assert!(holder.set_group_file_sharing_override(NetworkCost::Restricted, true));
    assert!(
        !outbound
            .lock()
            .unwrap()
            .events_excluding(&file, &Default::default())
            .unwrap()
            .is_empty()
    );
    holder
        .set_channel_replica_enabled(group, false)
        .await
        .unwrap();
    for store in [&inbound, &outbound] {
        assert!(
            store
                .lock()
                .unwrap()
                .events_excluding(&file, &Default::default())
                .unwrap()
                .is_empty(),
            "a temporary network exception must not override the saved sharing preference"
        );
    }
    assert_eq!(
        holder.log.lock().unwrap().events(&file).unwrap().len(),
        retained
    );
    let outsider = holder.serve_sync_store(&chain.nodes[6].identity.public());
    assert!(
        outsider
            .lock()
            .unwrap()
            .event_ids(&group)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn group_chain_waits_for_sharing_and_recovers_after_bridge_partition() {
    let mut chain = Chain::new().await;
    let group = chain.accepted_group().await;
    chain.nodes[2]
        .set_channel_replica_enabled(group, false)
        .await
        .unwrap();
    chain.nodes[0]
        .send_channel_message(group, b"held behind C")
        .await
        .unwrap();
    chain.recover_retained(1, group).await;
    chain.recover_retained(2, group).await;
    assert!(
        chain.has_text(2, group, b"held behind C"),
        "share off still receives its own copy"
    );
    chain.nodes[2].retry_channel_controls().await;
    // A local send must not smuggle the retained A message as causal ancestry.
    // With this append-only DAG it waits downstream until a sharing holder can
    // supply those missing parents; the local send remains durably accepted.
    let held = chain.nodes[0].channel_history(group, 100).unwrap()[0].id;
    chain.nodes[2]
        .send_channel_message(group, b"C while not sharing")
        .await
        .unwrap();
    for i in 3..6 {
        chain.recover_retained(i, group).await;
    }
    for i in 3..6 {
        assert!(!chain.has_text(i, group, b"held behind C"));
        assert!(
            !chain.nodes[i].log.lock().unwrap().has(&held).unwrap(),
            "not even retained user-message ciphertext crosses the barrier"
        );
        assert!(
            !chain.has_text(i, group, b"C while not sharing"),
            "own send waits for missing signed parents"
        );
    }
    chain.nodes[2]
        .set_channel_replica_enabled(group, true)
        .await
        .unwrap();
    chain.settle_retained(3, group).await;
    assert!(chain.has_text(3, group, b"held behind C"));
    // D stops serving before E copies the event. No other cross-site edge exists.
    let server = chain.servers[3].take().unwrap();
    server.abort();
    let _ = server.await;
    chain.recover_retained(4, group).await;
    chain.recover_retained(5, group).await;
    assert!(!chain.has_text(5, group, b"held behind C"));
    let listener = TcpListener::bind(chain.addresses[3]).await.unwrap();
    chain.servers[3] = Some(Chain::serve(Arc::clone(&chain.nodes[3]), listener));
    chain.recover_retained(4, group).await;
    chain.recover_retained(5, group).await;
    assert!(chain.has_text(5, group, b"held behind C"));
    assert!(
        chain.has_text(5, group, b"C while not sharing"),
        "deferred own send also recovers with its missing parents"
    );
}

#[tokio::test]
async fn unprocessed_durable_group_never_exposes_events_to_nonmember() {
    use crate::eventlog::sync::SyncStore;
    let chain = Chain::new().await;
    let group = chain.nodes[0]
        .create_channel_invited(
            "unprocessed",
            chain.nodes[1..6]
                .iter()
                .map(|n| n.identity.public())
                .collect(),
        )
        .await
        .unwrap();
    let signed = chain.nodes[0]
        .log
        .lock()
        .unwrap()
        .events(&group)
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>();
    for event in signed {
        chain.nodes[5].log.lock().unwrap().append(event).unwrap();
    }
    assert!(
        chain.nodes[5]
            .channels
            .lock()
            .unwrap()
            .state(&group)
            .is_none()
    );
    let store = chain.nodes[5].serve_sync_store(&chain.nodes[6].identity.public());
    {
        let store = store.lock().unwrap();
        assert_eq!(
            store.fingerprint_and_len(&group).unwrap(),
            (crate::eventlog::sync::fingerprint(Vec::new()), 0)
        );
        assert!(!store.requires_channel_v2(&group).unwrap());
        assert!(store.event_ids(&group).unwrap().is_empty());
        assert!(
            store
                .events_excluding(&group, &Default::default())
                .unwrap()
                .is_empty()
        );
    }
    let peer = chain.nodes[4].identity.public();
    let member_store = chain.nodes[5].serve_sync_store(&peer);
    let before = member_store
        .lock()
        .unwrap()
        .fingerprint_and_len(&group)
        .unwrap();
    assert!(
        before.1 > 0,
        "authorized invitation ancestry still bootstraps before book refresh"
    );
    chain.nodes[5].process_channel(group);
    chain.nodes[5].await_channel_metadata(group).await.unwrap();
    let replays = chain.nodes[5]
        .channel_projection_replays
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        member_store
            .lock()
            .unwrap()
            .fingerprint_and_len(&group)
            .unwrap(),
        before
    );
    assert_eq!(
        chain.nodes[5]
            .channel_projection_replays
            .load(std::sync::atomic::Ordering::Relaxed)
            - replays,
        1,
        "populating owner pin and members invalidates an unprocessed-log entry"
    );
}

#[tokio::test]
async fn public_group_idle_metadata_is_reused_across_fresh_peer_scoped_stores() {
    use crate::eventlog::sync::SyncStore;
    use std::sync::atomic::Ordering;
    let chain = Chain::new().await;
    let group = chain.accepted_group().await;
    chain.nodes[0]
        .set_channel_replica_enabled(group, true)
        .await
        .unwrap();
    let node = &chain.nodes[0];
    let before = node.channel_projection_replays.load(Ordering::Relaxed);
    let first = node.serve_sync_store(&chain.nodes[2].identity.public());
    let fp = first.lock().unwrap().fingerprint_and_len(&group).unwrap();
    assert!(first.lock().unwrap().requires_channel_v2(&group).unwrap());
    let fresh = node.serve_sync_store(&chain.nodes[2].identity.public());
    assert_eq!(
        fresh.lock().unwrap().fingerprint_and_len(&group).unwrap(),
        fp
    );
    assert_eq!(
        node.channel_projection_replays.load(Ordering::Relaxed) - before,
        1,
        "idle probes share one authorized metadata projection across new connections"
    );
}

#[tokio::test]
async fn public_group_history_capability_metadata_is_cached_and_principal_scoped() {
    use crate::{channel::HistoryPolicy, eventlog::sync::SyncStore};
    use std::sync::atomic::Ordering;
    let chain = Chain::new().await;
    let group = chain.accepted_group().await;
    let node = &chain.nodes[0];
    let member = chain.nodes[2].identity.public();
    // Default invitations already contain a bounded history offer.
    let original = node
        .serve_sync_store(&member)
        .lock()
        .unwrap()
        .fingerprint_and_len(&group)
        .unwrap();
    node.set_channel_history_policy(group, HistoryPolicy::LastTen)
        .await
        .unwrap();
    let before = node.channel_projection_replays.load(Ordering::Relaxed);
    for _ in 0..2 {
        let store = node.serve_sync_store(&member);
        let store = store.lock().unwrap();
        assert!(store.requires_channel_v3(&group).unwrap());
        assert!(!store.requires_channel_v2(&group).unwrap());
        assert_ne!(store.fingerprint_and_len(&group).unwrap(), original);
    }
    assert_eq!(
        node.channel_projection_replays.load(Ordering::Relaxed) - before,
        1
    );
    let outsider = node.serve_sync_store(&chain.nodes[6].identity.public());
    let outsider = outsider.lock().unwrap();
    assert!(!outsider.requires_channel_v3(&group).unwrap());
    assert!(outsider.event_ids(&group).unwrap().is_empty());
}

#[tokio::test]
async fn public_group_metadata_invalidates_for_authority_history_and_privacy_changes() {
    use crate::{
        discovery::visibility::DiscoveryVisibility,
        eventlog::{EventKind, sync::SyncStore},
        identity::device::PublicIdentity,
    };
    use std::sync::atomic::Ordering;
    fn metadata(
        node: &Node,
        peer: &PublicIdentity,
        group: ConversationId,
    ) -> (([u8; 32], usize), bool) {
        let store = node.serve_sync_store(peer);
        let store = store.lock().unwrap();
        (
            store.fingerprint_and_len(&group).unwrap(),
            store.requires_channel_v2(&group).unwrap(),
        )
    }
    let chain = Chain::new().await;
    let group = chain.accepted_group().await;
    let node = &chain.nodes[0];
    let member = chain.nodes[2].identity.public();
    let original = metadata(node, &member, group);
    assert!(original.0.1 > 0);
    assert!(!original.1);
    assert_eq!(
        metadata(node, &chain.nodes[6].identity.public(), group).0.1,
        0,
        "cached member metadata never crosses principals"
    );
    let mut wrong_static = member.clone();
    wrong_static.x25519_pub = chain.nodes[6].identity.public().x25519_pub;
    assert_eq!(
        metadata(node, &wrong_static, group).0.1,
        0,
        "cache binds both identity keys"
    );

    node.send_channel_message(group, b"cache append")
        .await
        .unwrap();
    assert_ne!(
        metadata(node, &member, group).0,
        original.0,
        "durable append invalidates digest metadata"
    );
    node.append_event(group, EventKind::ChannelReplica, vec![0])
        .unwrap();
    let before_book_refresh = metadata(node, &member, group);
    assert!(before_book_refresh.0.1 > 0);
    assert!(before_book_refresh.1);
    node.process_channel(group);
    node.await_channel_metadata(group).await.unwrap();
    assert_eq!(
        metadata(node, &member, group).0.1,
        0,
        "sharing policy refresh invalidates even without another log append"
    );
    node.set_channel_replica_enabled(group, true).await.unwrap();
    assert!(metadata(node, &member, group).0.1 > 0);

    // Configuration can change without a generation increment. Public/private
    // mode and configured state are independent key inputs.
    let directory = chain._dir.path().join("privacy");
    std::fs::create_dir_all(&directory).unwrap();
    node.configure_privacy(
        &directory,
        "pw",
        &node.signed_announce("A", chain.addresses[0].port()),
        Arc::new(DiscoveryVisibility::new(true)),
    )
    .unwrap();
    let before = node.channel_projection_replays.load(Ordering::Relaxed);
    assert!(metadata(node, &member, group).0.1 > 0);
    assert_eq!(
        node.channel_projection_replays.load(Ordering::Relaxed) - before,
        1,
        "new public configuration does not reuse unconfigured entry"
    );
    node.set_invisible(true).await.unwrap();
    let before = node.channel_projection_replays.load(Ordering::Relaxed);
    let private = metadata(node, &member, group);
    assert!(
        node.channel_projection_replays.load(Ordering::Relaxed) > before,
        "invisible proofs are replayed, never read from the public cache"
    );
    let before = node.channel_projection_replays.load(Ordering::Relaxed);
    assert_eq!(metadata(node, &member, group), private);
    assert!(
        node.channel_projection_replays.load(Ordering::Relaxed) > before,
        "even unchanged private metadata replays current proofs"
    );
    node.set_invisible(false).await.unwrap();
    assert!(metadata(node, &member, group).0.1 > 0);
    let before = node.channel_projection_replays.load(Ordering::Relaxed);
    node.privacy.generation.fetch_add(1, Ordering::SeqCst);
    assert!(metadata(node, &member, group).0.1 > 0);
    assert_eq!(
        node.channel_projection_replays.load(Ordering::Relaxed) - before,
        1,
        "a changed public privacy generation invalidates metadata"
    );

    node.remove_channel_member(group, &member.user_id())
        .await
        .unwrap();
    assert_eq!(
        metadata(node, &member, group).0.1,
        0,
        "removal invalidates a former member's metadata"
    );
    let remaining = chain.nodes[3].identity.public();
    assert!(metadata(node, &remaining, group).0.1 > 0);
    node.log
        .lock()
        .unwrap()
        .retain_conversations(&Default::default())
        .unwrap();
    assert_eq!(
        metadata(node, &remaining, group).0.1,
        0,
        "whole-conversation compaction cannot expose erased metadata"
    );
    assert!(!metadata(node, &remaining, group).1);
}

#[tokio::test]
async fn public_group_metadata_cache_is_bounded_under_distinct_authenticated_principals() {
    use crate::eventlog::sync::SyncStore;
    let chain = Chain::new().await;
    let group = chain.accepted_group().await;
    for _ in 0..160 {
        let outsider = DeviceIdentity::generate().public();
        let store = chain.nodes[0].serve_sync_store(&outsider);
        assert_eq!(
            store.lock().unwrap().fingerprint_and_len(&group).unwrap().1,
            0
        );
    }
    assert_eq!(
        chain.nodes[0]
            .channel_projection_cache
            .lock()
            .unwrap()
            .len(),
        128
    );
    let member = chain.nodes[1].identity.public();
    let store = chain.nodes[0].serve_sync_store(&member);
    assert!(
        store.lock().unwrap().fingerprint_and_len(&group).unwrap().1 > 0,
        "eviction cannot change member authority"
    );
    assert!(
        chain.nodes[0]
            .channel_projection_cache
            .lock()
            .unwrap()
            .len()
            <= 128
    );
}

#[tokio::test]
async fn established_group_owner_pin_rejects_a_competing_signed_genesis() {
    use crate::{
        channel::ChannelMeta,
        eventlog::{Event, EventKind, sync::SyncStore},
    };
    let chain = Chain::new().await;
    let group = chain.accepted_group().await;
    chain.nodes[0]
        .send_channel_message(group, b"legitimate group content")
        .await
        .unwrap();
    let original = chain.nodes[0].channel_history(group, 100).unwrap()[0].id;
    let attacker = &chain.nodes[6];
    // Valid signature and self-consistent metadata do not confer authority over
    // an existing conversation id. A lower Lamport makes the rival root replay
    // first deterministically; ChannelBook still pins the genuine genesis owner.
    let rival = ChannelMeta {
        name: "forged enrollment".into(),
        members: vec![
            chain.nodes[0].identity.public(),
            chain.nodes[2].identity.public(),
            attacker.identity.public(),
        ],
        epoch: 0,
        owner: attacker.user_id(),
    };
    let forged = Event::new(
        &attacker.identity,
        group,
        1,
        vec![],
        0,
        0,
        EventKind::MembershipChange,
        rival.encode(),
    );
    let forged_id = forged.id;
    chain.nodes[0].log.lock().unwrap().append(forged).unwrap();
    let authorized = chain.nodes[0].serve_sync_store(&chain.nodes[2].identity.public());
    let ids = authorized.lock().unwrap().event_ids(&group).unwrap();
    assert!(
        !ids.contains(&forged_id),
        "a competing root cannot replace established channel authority"
    );
    assert!(
        ids.contains(&original),
        "the real group's content remains available to accepted members"
    );
    let unauthorized = chain.nodes[0].serve_sync_store(&attacker.identity.public());
    assert_eq!(
        unauthorized
            .lock()
            .unwrap()
            .fingerprint_and_len(&group)
            .unwrap()
            .1,
        0
    );
    assert!(
        unauthorized
            .lock()
            .unwrap()
            .events_excluding(&group, &Default::default())
            .unwrap()
            .is_empty()
    );
    // Manual account permission in Invisible mode still cannot grant the
    // permitted attacker authority to replace this group's genesis owner.
    let owner = &chain.nodes[0];
    owner.roster.lock().unwrap().update(
        &attacker.signed_announce("attacker", chain.addresses[6].port()),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &owner.user_id(),
    );
    let directory = chain._dir.path().join("pin-privacy");
    std::fs::create_dir_all(&directory).unwrap();
    owner
        .configure_privacy(
            &directory,
            "pw",
            &owner.signed_announce("owner", chain.addresses[0].port()),
            Arc::new(crate::discovery::visibility::DiscoveryVisibility::new(true)),
        )
        .unwrap();
    owner.cached_peer_snapshot().unwrap();
    owner
        .set_allowed(&attacker.account_id(), true)
        .await
        .unwrap();
    owner.set_invisible(true).await.unwrap();
    let private_member = owner.serve_sync_store(&chain.nodes[2].identity.public());
    let ids = private_member.lock().unwrap().event_ids(&group).unwrap();
    assert!(ids.contains(&original));
    assert!(!ids.contains(&forged_id));
    let private_attacker = owner.serve_sync_store(&attacker.identity.public());
    assert!(
        private_attacker
            .lock()
            .unwrap()
            .event_ids(&group)
            .unwrap()
            .is_empty()
    );
}

// These independent hosts use synchronous durable storage. Match the production
// multithread executor without changing the network or post-send time budgets.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_group_gossip_reaches_end_of_chain_without_sender_or_roster_changes() {
    let mut chain = Chain::new().await;
    let group = chain.accepted_group().await;
    // Setup copied the signed controls without a recovery worker. Complete only
    // that retained work before measuring propagation of a new message.
    for index in 0..6 {
        chain.settle_retained(index, group).await;
        let projection = chain.nodes[index].account_group_projection(group).unwrap();
        let state = projection.state.as_ref().unwrap();
        for member in &chain.nodes[..6] {
            assert!(state.is_accepted_member(&member.user_id()));
        }
    }
    let mut recovery_before = Vec::with_capacity(6);
    // Switch the prepared, fully accepted membership to the complete production
    // accept/recovery runtime. No manual recovery calls occur after this point.
    for index in 0..6 {
        let previous = chain.servers[index].take().unwrap();
        previous.abort();
        let _ = previous.await;
        recovery_before.push(
            chain.nodes[index]
                .channel_recovery(group)
                .and_then(|recovery| recovery.checked_at),
        );
        let listener = TcpListener::bind(chain.addresses[index]).await.unwrap();
        chain.servers[index] = Some(tokio::spawn(
            chain.nodes[index].clone().run_accept_loop(listener),
        ));
    }
    // Require a new clean authenticated exchange by each replacement worker;
    // historical peer_checked state cannot satisfy this setup barrier. This
    // setup deadline is separate from the unchanged post-message two seconds.
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut initialized = [false; 6];
        loop {
            for (index, node) in chain.nodes[..6].iter().enumerate() {
                initialized[index] |= node.channel_recovery(group).is_some_and(|recovery| {
                    recovery.phase == "peer_checked"
                        && recovery.checked_at.is_some_and(|checked| {
                            recovery_before[index].is_none_or(|before| checked > before)
                        })
                });
            }
            if initialized.into_iter().all(|ready| ready) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("prepared adjacent endpoints must finish their initial background exchange");
    let route_trace = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let route_started = std::time::Instant::now();
    for (index, node) in chain.nodes[..6].iter().enumerate() {
        let observed_route = route_trace.clone();
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new(move |peer, phase| {
            // Include the pooled-session path, which has no dial_* callbacks.
            if !phase.starts_with("channel_recovery_") && !phase.starts_with("dial_") {
                return;
            }
            let mut trace = observed_route.lock().unwrap();
            if trace.len() == 512 {
                trace.pop_front();
            }
            trace.push_back((
                route_started.elapsed().as_millis(),
                index,
                peer.user_id(),
                tokio::task::try_id(),
                phase,
            ));
        }));
    }
    chain.nodes[0]
        .send_channel_message(group, b"notification-driven background gossip")
        .await
        .unwrap();
    let sender = chain.servers[0].take().unwrap();
    sender.abort();
    let _ = sender.await;
    // A live worker can invalidate a read checkpoint between polling turns.
    // Only that typed pending state is retried, within the original deadline;
    // no manual projection or network recovery is driven by these pure reads.
    let received = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let all_received =
                (1..6).all(
                    |index| match chain.nodes[index].channel_history(group, 100) {
                        Ok(history) => history
                            .iter()
                            .any(|row| row.text == b"notification-driven background gossip"),
                        Err(super::NodeError::Log(crate::eventlog::LogError::StaleCursor)) => false,
                        Err(error) => panic!("background history read failed: {error}"),
                    },
                );
            if all_received {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    for node in &chain.nodes[..6] {
        *node.account_client_test_hook.lock().unwrap() = None;
    }
    if received.is_err() {
        // These additional snapshot reads occur only after the deadline;
        // do not consume Notify permits or invoke recovery here.
        let original = chain.nodes[0]
            .channel_history(group, 10)
            .unwrap()
            .into_iter()
            .find(|row| row.text == b"notification-driven background gossip")
            .map(|row| row.id);
        let observations: Vec<_> = chain
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let (events, present) = {
                    let log = node.log.lock().unwrap();
                    (
                        log.events(&group).unwrap().len(),
                        original.is_some_and(|id| log.get(&id).unwrap().is_some()),
                    )
                };
                let surfaced = original.is_some_and(|id| {
                    node.channel_history(group, 10)
                        .unwrap()
                        .iter()
                        .any(|row| row.id == id)
                });
                let queued = node
                    .channel_recovery_pending
                    .lock()
                    .unwrap()
                    .contains(&group);
                let recovery = node.channel_recovery.lock().unwrap().get(&group).cloned();
                (
                    index,
                    events,
                    present,
                    surfaced,
                    queued,
                    recovery,
                    node.sessions.test_capacity_snapshot(),
                )
            })
            .collect();
        eprintln!(
            "background group gossip metadata original={original:?} observations={observations:?}"
        );
        eprintln!(
            "bounded recovery phases={:?}",
            route_trace.try_lock().ok().as_deref()
        );
    }
    received.expect("new validated messages must wake bounded background gossip rather than wait several three-second ticks");
}

#[tokio::test]
async fn channel_recovery_wake_is_deduplicated_and_empty_rounds_stay_quiet() {
    async fn finish_progress(node: &Node, group: ConversationId) {
        use futures_util::FutureExt;
        // This fixture has no recovery worker. Finish its admitted setup/message
        // continuations before testing idle rounds, consuming their progress wake.
        for _ in 0..128 {
            node.process_channel_reception_inner(group, "live", false)
                .unwrap();
            if node
                .channel_checkpoints
                .next_runtime(None)
                .unwrap()
                .is_none()
            {
                let _ = node.channel_recovery_notify.notified().now_or_never();
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("fixture reception must finish its bounded continuation");
    }
    let chain = Chain::new().await;
    let group = chain.accepted_group().await;
    let receiver = &chain.nodes[1];
    finish_progress(receiver, group).await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            receiver.channel_recovery_notify.notified()
        )
        .await
        .is_err(),
        "membership setup and empty rounds do not signal message gossip"
    );
    chain.nodes[0]
        .send_channel_message(group, b"wake once")
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        receiver.channel_recovery_notify.notified(),
    )
    .await
    .expect("validated received message signals the dedicated recovery notification");
    finish_progress(receiver, group).await;
    for _ in 0..3 {
        receiver.process_channel(group);
    }
    chain.round(1, 0, group).await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            receiver.channel_recovery_notify.notified()
        )
        .await
        .is_err(),
        "already surfaced messages and empty anti-entropy rounds must not self-wake"
    );
}

#[tokio::test]
async fn owned_read_failure_retains_untracked_group_chunks_and_existing_media() {
    let chain = Chain::new().await;
    let group = chain.accepted_group().await;
    let source = chain._dir.path().join("reclamation.png");
    let mut bytes = vec![7; crate::file::CHUNK_SIZE + 31];
    bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    std::fs::write(&source, &bytes).unwrap();
    let file = chain.nodes[0]
        .send_file_channel(group, &source, crate::file::FileKind::Media)
        .await
        .unwrap();
    chain.round(1, 0, group).await;
    chain.nodes[1].process_file_events(group);
    chain.round(1, 0, file).await;
    let receiver = &chain.nodes[1];
    receiver
        .set_channel_replica_enabled(group, false)
        .await
        .unwrap();
    receiver
        .media
        .store_bytes(file, "reclamation.png", &bytes)
        .unwrap();
    let path = receiver.media.path(file).unwrap();
    let encrypted_before = std::fs::read(&path).unwrap();
    let chunks_before = receiver.log.lock().unwrap().events(&file).unwrap().len();
    assert!(chunks_before > 0);
    assert_eq!(receiver.completion_candidates(file).unwrap().len(), 1);
    // Current group reception reserves completion evidence. The fallback still
    // has to preserve older/imported cards that lack this optional binding.
    assert_eq!(
        receiver
            .delivery
            .lock()
            .unwrap()
            .omit_file_completion_binding_for_test(file),
        1
    );
    assert!(
        receiver.completion_candidates(file).unwrap().is_empty(),
        "legacy group reception uses the untracked fallback"
    );
    let mut reached_completion = false;
    let result =
        receiver.save_file_progress(file, &chain._dir.path().join("export.png"), |progress| {
            if progress.done == progress.total {
                reached_completion = true;
                receiver.log.lock().unwrap().fail_reads(true);
            }
        });
    assert!(
        reached_completion,
        "arm failure only after the final chunk read"
    );
    assert!(matches!(
        result,
        Err(super::NodeError::Log(crate::eventlog::LogError::Io(_)))
    ));
    // Inspect durable state only after the simulated storage outage has ended.
    // Indexed reads correctly observe the same failure as the failed reclaim.
    receiver.log.lock().unwrap().fail_reads(false);
    assert_eq!(
        receiver.log.lock().unwrap().events(&file).unwrap().len(),
        chunks_before
    );
    assert!(
        std::fs::read(&path).unwrap() == encrypted_before,
        "failed reclaim must preserve encrypted managed bytes"
    );
    assert!(
        receiver.media.read(file).unwrap() == bytes,
        "retained media must decrypt to complete original bytes"
    );
    let checks_before_retry = receiver
        .untracked_chunk_validations
        .load(std::sync::atomic::Ordering::Relaxed);
    receiver
        .save_file(file, &chain._dir.path().join("export-retry.png"))
        .unwrap();
    assert!(
        receiver
            .untracked_chunk_validations
            .load(std::sync::atomic::Ordering::Relaxed)
            > checks_before_retry,
        "a real untracked retry must verify retained chunk ancestry"
    );
    assert!(
        receiver
            .log
            .lock()
            .unwrap()
            .events(&file)
            .unwrap()
            .is_empty(),
        "successful share-off reclaim removes the same chunks after storage recovers"
    );
}
