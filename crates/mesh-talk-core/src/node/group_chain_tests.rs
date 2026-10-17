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
            let node = Node::open(
                DeviceIdentity::generate(),
                Arc::new(Mutex::new(Roster::default())),
                dm,
                channel,
                file,
                &dir.path().join(format!("{i}-events.log")),
                &dir.path().join(format!("{i}-sent.log")),
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
                    .iter()
                    .any(|c| c.id == channel && !c.pending_invite)
            );
        }
        assert_eq!(self.nodes[0].roster.lock().unwrap().peers().len(), 1);
        channel
    }

    async fn recover(&self, index: usize) {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.nodes[index].pull_one_channel_from_member(0),
        )
        .await
        .expect("bounded adjacent-member recovery");
    }

    fn has_text(&self, index: usize, channel: ConversationId, text: &[u8]) -> bool {
        self.nodes[index]
            .channel_history(channel, 100)
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
        chain.recover(i).await;
        assert!(
            chain.has_text(i, group, b"A to F through members"),
            "member {i} decrypts original author's event"
        );
    }
    let original = chain.nodes[0].channel_history(group, 100)[0].id;
    for node in &chain.nodes[1..6] {
        assert_eq!(node.channel_history(group, 100)[0].id, original);
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
    chain.recover(5).await;
    assert_eq!(chain.nodes[5].channel_history(group, 100).len(), 1);
    // Removal rotates the epoch. Even a formerly accepted member cannot
    // recover the later sender key or ciphertext by guessing the group id.
    chain.nodes[0]
        .remove_channel_member(group, &chain.nodes[5].user_id())
        .await
        .unwrap();
    for i in 1..5 {
        chain.recover(i).await;
    }
    chain.nodes[0]
        .send_channel_message(group, b"after F removal")
        .await
        .unwrap();
    for i in 1..5 {
        chain.recover(i).await;
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
    chain.recover(1).await;
    chain.recover(2).await;
    assert!(
        chain.has_text(2, group, b"held behind C"),
        "share off still receives its own copy"
    );
    chain.nodes[2].retry_channel_controls().await;
    // A local send must not smuggle the retained A message as causal ancestry.
    // With this append-only DAG it waits downstream until a sharing holder can
    // supply those missing parents; the local send remains durably accepted.
    let held = chain.nodes[0].channel_history(group, 100)[0].id;
    chain.nodes[2]
        .send_channel_message(group, b"C while not sharing")
        .await
        .unwrap();
    for i in 3..6 {
        chain.recover(i).await;
    }
    for i in 3..6 {
        assert!(!chain.has_text(i, group, b"held behind C"));
        assert!(
            !chain.nodes[i].log.lock().unwrap().has(&held),
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
    chain.nodes[3].process_channel(group);
    assert!(chain.has_text(3, group, b"held behind C"));
    // D stops serving before E copies the event. No other cross-site edge exists.
    let server = chain.servers[3].take().unwrap();
    server.abort();
    let _ = server.await;
    chain.recover(4).await;
    chain.recover(5).await;
    assert!(!chain.has_text(5, group, b"held behind C"));
    let listener = TcpListener::bind(chain.addresses[3]).await.unwrap();
    chain.servers[3] = Some(Chain::serve(Arc::clone(&chain.nodes[3]), listener));
    chain.recover(4).await;
    chain.recover(5).await;
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
        .into_iter()
        .cloned()
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
    let store = store.lock().unwrap();
    assert_eq!(
        store.fingerprint_and_len(&group),
        (crate::eventlog::sync::fingerprint(Vec::new()), 0)
    );
    assert!(!store.requires_channel_v2(&group));
    assert!(store.event_ids(&group).is_empty());
    assert!(
        store
            .events_excluding(&group, &Default::default())
            .is_empty()
    );
    drop(store);
    let peer = chain.nodes[4].identity.public();
    let member_store = chain.nodes[5].serve_sync_store(&peer);
    let before = member_store.lock().unwrap().fingerprint_and_len(&group);
    assert!(
        before.1 > 0,
        "authorized invitation ancestry still bootstraps before book refresh"
    );
    chain.nodes[5].process_channel(group);
    let replays = chain.nodes[5]
        .channel_projection_replays
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        member_store.lock().unwrap().fingerprint_and_len(&group),
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
    let fp = first.lock().unwrap().fingerprint_and_len(&group);
    assert!(first.lock().unwrap().requires_channel_v2(&group));
    let fresh = node.serve_sync_store(&chain.nodes[2].identity.public());
    assert_eq!(fresh.lock().unwrap().fingerprint_and_len(&group), fp);
    assert_eq!(
        node.channel_projection_replays.load(Ordering::Relaxed) - before,
        1,
        "idle probes share one authorized metadata projection across new connections"
    );
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
            store.fingerprint_and_len(&group),
            store.requires_channel_v2(&group),
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
        assert_eq!(store.lock().unwrap().fingerprint_and_len(&group).1, 0);
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
        store.lock().unwrap().fingerprint_and_len(&group).1 > 0,
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
    let original = chain.nodes[0].channel_history(group, 100)[0].id;
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
    let ids = authorized.lock().unwrap().event_ids(&group);
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
        unauthorized.lock().unwrap().fingerprint_and_len(&group).1,
        0
    );
    assert!(
        unauthorized
            .lock()
            .unwrap()
            .events_excluding(&group, &Default::default())
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
    let ids = private_member.lock().unwrap().event_ids(&group);
    assert!(ids.contains(&original));
    assert!(!ids.contains(&forged_id));
    let private_attacker = owner.serve_sync_store(&attacker.identity.public());
    assert!(
        private_attacker
            .lock()
            .unwrap()
            .event_ids(&group)
            .is_empty()
    );
}

#[tokio::test]
async fn background_group_gossip_reaches_end_of_chain_without_sender_or_roster_changes() {
    let mut chain = Chain::new().await;
    let group = chain.accepted_group().await;
    // Switch the prepared, fully accepted membership to the complete production
    // accept/recovery runtime. No manual recovery calls occur after this point.
    for index in 0..6 {
        let previous = chain.servers[index].take().unwrap();
        previous.abort();
        let _ = previous.await;
        let listener = TcpListener::bind(chain.addresses[index]).await.unwrap();
        chain.servers[index] = Some(tokio::spawn(
            chain.nodes[index].clone().run_accept_loop(listener),
        ));
    }
    // Let startup and its immediately-ready interval tick finish before sending.
    tokio::time::sleep(Duration::from_millis(500)).await;
    chain.nodes[0]
        .send_channel_message(group, b"notification-driven background gossip")
        .await
        .unwrap();
    let sender = chain.servers[0].take().unwrap();
    sender.abort();
    let _ = sender.await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while !chain.has_text(5, group, b"notification-driven background gossip") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("new validated messages must wake bounded background gossip rather than wait several three-second ticks");
    for index in 1..6 {
        assert!(chain.has_text(index, group, b"notification-driven background gossip"));
    }
}

#[tokio::test]
async fn channel_recovery_wake_is_deduplicated_and_empty_rounds_stay_quiet() {
    let chain = Chain::new().await;
    let group = chain.accepted_group().await;
    let receiver = &chain.nodes[1];
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
