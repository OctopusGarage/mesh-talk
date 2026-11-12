//! Runtime routing and profile tests.

use super::*;

struct FixedCandidates(std::net::SocketAddr);

impl crate::discovery::candidates::CandidateSource for FixedCandidates {
    fn snapshot(&self) -> futures_util::future::BoxFuture<'_, Vec<std::net::SocketAddr>> {
        Box::pin(async move { vec![self.0] })
    }
}

#[tokio::test]
async fn runtime_automatically_queries_a_host_supplied_routed_candidate() {
    let root = tempfile::tempdir().unwrap();
    let reserve = crate::transport::net::discovery_socket(0).unwrap();
    let b_port = reserve.local_addr().unwrap().port();
    drop(reserve);
    let b = NodeRuntime::start(
        root.path(),
        "bob",
        "Bob",
        "pw",
        b_port,
        |_| {},
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await
    .unwrap();
    let reserve = crate::transport::net::discovery_socket(0).unwrap();
    let a_port = reserve.local_addr().unwrap().port();
    drop(reserve);
    let source = Arc::new(FixedCandidates(std::net::SocketAddr::from((
        [127, 0, 0, 1],
        b_port,
    ))));
    let a = NodeRuntime::start_configured(
        RuntimeConfig {
            base_dir: root.path(),
            account_id: "alice",
            display_name: "Alice",
            password: "pw",
            discovery_port: a_port,
            post_office_enabled: false,
            candidate_source: Some(source),
        },
        RuntimeEvents {
            on_dm: Box::new(|_| {}),
            on_channel: Box::new(|_| {}),
            on_file: Box::new(|_| {}),
            on_profile: Box::new(|_| {}),
            on_call_signal: Box::new(|_| {}),
        },
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(4), async {
        while !a
            .peers()
            .iter()
            .any(|peer| peer.public.user_id() == b.user_id())
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("candidate query discovered peer");
    a.stop().await;
    b.stop().await;
}

fn reserve_dual_discovery_port() -> (tokio::net::UdpSocket, tokio::net::UdpSocket) {
    for _ in 0..32 {
        let ipv4 = crate::transport::net::discovery_socket(0).unwrap();
        match routed_ipv6_socket(ipv4.local_addr().unwrap().port()) {
            Ok(ipv6) => return (ipv4, ipv6),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(error) => panic!("reserve the routed discovery fixture: {error}"),
        }
    }
    panic!("reserve a discovery port available in both address families");
}

#[tokio::test]
async fn runtime_automatically_queries_ipv6_candidate_when_ipv6_is_available() {
    let Ok(available) = routed_ipv6_socket(0) else {
        return;
    };
    drop(available);
    // IPv6-only allocation does not reserve the IPv4 port the runtime binds
    // first. Retain both compatible reuse sockets until startup owns them.
    let b_reservation = reserve_dual_discovery_port();
    let b_port = b_reservation.0.local_addr().unwrap().port();
    let root = tempfile::tempdir().unwrap();
    let b = NodeRuntime::start(
        root.path(),
        "bob",
        "Bob",
        "pw",
        b_port,
        |_| {},
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await
    .unwrap();
    drop(b_reservation);
    let a_reservation = reserve_dual_discovery_port();
    let a_port = a_reservation.0.local_addr().unwrap().port();
    let source = Arc::new(FixedCandidates(std::net::SocketAddr::from((
        std::net::Ipv6Addr::LOCALHOST,
        b_port,
    ))));
    let a = NodeRuntime::start_configured(
        RuntimeConfig {
            base_dir: root.path(),
            account_id: "alice",
            display_name: "Alice",
            password: "pw",
            discovery_port: a_port,
            post_office_enabled: false,
            candidate_source: Some(source),
        },
        RuntimeEvents {
            on_dm: Box::new(|_| {}),
            on_channel: Box::new(|_| {}),
            on_file: Box::new(|_| {}),
            on_profile: Box::new(|_| {}),
            on_call_signal: Box::new(|_| {}),
        },
    )
    .await
    .unwrap();
    drop(a_reservation);
    tokio::time::timeout(Duration::from_secs(4), async {
        while !a
            .peers()
            .iter()
            .any(|peer| peer.public.user_id() == b.user_id())
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("IPv6 candidate query discovered peer");
    a.stop().await;
    b.stop().await;
}

#[tokio::test]
async fn runtime_manual_endpoint_probe_uses_the_same_query_protocol() {
    let root = tempfile::tempdir().unwrap();
    let reserve = crate::transport::net::discovery_socket(0).unwrap();
    let b_port = reserve.local_addr().unwrap().port();
    drop(reserve);
    let b = NodeRuntime::start(
        root.path(),
        "bob",
        "Bob",
        "pw",
        b_port,
        |_| {},
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await
    .unwrap();
    let reserve = crate::transport::net::discovery_socket(0).unwrap();
    let a_port = reserve.local_addr().unwrap().port();
    drop(reserve);
    let a = NodeRuntime::start(
        root.path(),
        "alice",
        "Alice",
        "pw",
        a_port,
        |_| {},
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await
    .unwrap();
    a.probe_endpoint(std::net::SocketAddr::from(([127, 0, 0, 1], b_port)))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !a
            .peers()
            .iter()
            .any(|peer| peer.public.user_id() == b.user_id())
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("manual candidate discovered peer");
    a.stop().await;
    b.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_node_or_privacy_initialization_joins_already_started_names_writer() {
    struct ReleaseNames(Option<std::sync::mpsc::Sender<()>>);
    impl Drop for ReleaseNames {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    for failure_path in ["messages.log", "privacy.policy"] {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("accounts").join("owner");
        std::fs::create_dir_all(&directory).unwrap();
        // Regular paths pass source validation. The corrupt content fails in
        // the actual Node/privacy initializer after names work has started.
        std::fs::write(
            directory.join(failure_path),
            b"invalid legacy source header",
        )
        .unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut release = ReleaseNames(Some(release_tx));
        NAMES_OPEN_HOOKS.lock().unwrap().insert(
            directory.join("names.log"),
            Box::new(move || {
                let _ = entered_tx.send(());
                release_rx.recv().unwrap();
            }),
        );
        let base = root.path().to_owned();
        let (finished_tx, mut finished_rx) = tokio::sync::oneshot::channel();
        let startup = tokio::spawn(async move {
            let result = NodeRuntime::start(
                &base,
                "owner",
                "Owner",
                "pw",
                0,
                |_| {},
                |_| {},
                |_| {},
                |_| {},
                |_| {},
            )
            .await;
            finished_tx.send(()).unwrap();
            result
        });
        let entered = tokio::time::timeout(Duration::from_secs(5), entered_rx).await;
        if entered.is_err() {
            // Release before removing the hook: it may have just started and
            // still hold the hook-map lock while waiting on this channel.
            drop(release);
            NAMES_OPEN_HOOKS
                .lock()
                .unwrap()
                .remove(&directory.join("names.log"));
            panic!("the valid source paths must reach the names initializer");
        }
        entered.unwrap().unwrap();
        let returned_while_writer_blocked =
            tokio::time::timeout(Duration::from_millis(150), &mut finished_rx)
                .await
                .is_ok();
        release.0.take().unwrap().send(()).unwrap();
        assert!(startup.await.unwrap().is_err());
        assert!(
            !returned_while_writer_blocked,
            "{failure_path} error detached the paused names writer"
        );
        // Completion includes the real NameDirectory::open append/writer, not
        // just the gate callback: both handles were actually joined.
        assert!(directory.join("names.log").is_file());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn names_initialization_error_waits_for_node_initializer_completion() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("accounts").join("owner");
    std::fs::create_dir_all(directory.join("names.log")).unwrap();
    let result = NodeRuntime::start(
        root.path(),
        "owner",
        "Owner",
        "pw",
        0,
        |_| {},
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await;
    assert!(result.is_err());
    // Profile source validation rejects the invalid names path before database
    // publication. Completion includes the failed initializer, not a detached
    // writer that could create the database after this error returns.
    assert!(!directory.join("profile.db").exists());
    // Remove only the deliberately invalid names path, then reopen this same
    // profile immediately: the failed startup must have joined its Node writer.
    std::fs::remove_dir(directory.join("names.log")).unwrap();
    let node = NodeRuntime::start(
        root.path(),
        "owner",
        "Owner",
        "pw",
        0,
        |_| {},
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await
    .unwrap();
    node.stop().await;
}

fn fake_peer(post_office: bool) -> PeerRecord {
    PeerRecord {
        public: crate::identity::device::DeviceIdentity::generate().public(),
        addr: "127.0.0.1:1".parse().unwrap(),
        routes: Vec::new(),
        name: "p".into(),
        post_office,
        account_id: None,
        last_seen: std::time::Instant::now(),
    }
}

#[test]
fn channel_invitees_exclude_post_office_nodes() {
    assert!(channel_invitee_identity(fake_peer(true)).is_none());
    assert!(channel_invitee_identity(fake_peer(false)).is_some());
}

#[tokio::test]
async fn profile_republish_master_relay_enable_bypasses_cooldown() {
    profile_relay_dependency_change(true).await;
}

#[tokio::test]
async fn profile_republish_new_relay_bypasses_cooldown() {
    profile_relay_dependency_change(false).await;
}

async fn profile_relay_dependency_change(enable_existing: bool) {
    use crate::discovery::Announce;
    use crate::identity::{account::Account, device::DeviceIdentity};
    let root = tempfile::tempdir().unwrap();
    let recipient = DeviceIdentity::generate();
    let recipient_proof =
        Announce::new_with_account(&recipient, &Account::generate(), "Recipient", 21212);
    let (node, _, _) = crate::node::test_support::node_with_files(
        root.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &recipient_proof,
    );
    let relay = DeviceIdentity::generate();
    let relay_proof =
        Announce::new_post_office_with_account(&relay, &Account::generate(), "Relay", 21213);
    let recipient_peer = node
        .roster
        .lock()
        .unwrap()
        .get(&recipient.user_id())
        .unwrap()
        .clone();
    if enable_existing {
        node.roster.lock().unwrap().update(
            &relay_proof,
            "127.0.0.1".parse().unwrap(),
            &node.user_id(),
        );
        node.set_post_office_enabled(false);
    } else {
        node.set_post_office_enabled(true);
    }
    let now = std::time::Instant::now();
    let mut relay_observation = ProfileRelayObservation::default();
    let scope = profile_publication_scope(&node, &mut relay_observation, now);
    let mut published = std::collections::HashMap::new();
    assert_eq!(
        profile_republish_targets(
            &mut published,
            std::slice::from_ref(&recipient_peer),
            1,
            scope,
            now
        )
        .len(),
        1
    );
    let owner = published[&recipient.user_id()].owner.clone();
    finish_profile_publication(&mut published, &recipient.user_id(), &owner, false, now);
    assert!(
        profile_republish_targets(
            &mut published,
            std::slice::from_ref(&recipient_peer),
            1,
            scope,
            now
        )
        .is_empty()
    );
    if enable_existing {
        node.set_post_office_enabled(true);
    } else {
        node.roster.lock().unwrap().update(
            &relay_proof,
            "127.0.0.1".parse().unwrap(),
            &node.user_id(),
        );
    }
    assert!(node.relay_send_allowed(&relay.public()));
    assert_eq!(
        profile_republish_targets(
            &mut published,
            &[recipient_peer],
            1,
            profile_publication_scope(&node, &mut relay_observation, now),
            now
        )
        .len(),
        1,
        "newly available relay publication must not wait for an unchanged recipient's old cooldown"
    );
}

#[tokio::test]
async fn profile_republish_actual_relay_scope_preserves_heartbeats_and_admits_recovery() {
    use crate::discovery::Announce;
    use crate::identity::{account::Account, device::DeviceIdentity};
    let root = tempfile::tempdir().unwrap();
    let recipient = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&recipient, &Account::generate(), "Recipient", 21212);
    let (node, _, _) = crate::node::test_support::node_with_files(
        root.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let relay = DeviceIdentity::generate();
    let account = Account::generate();
    let proof = Announce::new_post_office_with_account(&relay, &account, "Relay", 21213);
    let ip = "127.0.0.1".parse().unwrap();
    node.roster
        .lock()
        .unwrap()
        .update_routed(&proof, ip, &node.user_id());
    let now = std::time::Instant::now();
    let original_deadline = node
        .roster
        .lock()
        .unwrap()
        .get(&relay.user_id())
        .unwrap()
        .presence_deadline(now)
        .unwrap();
    let mut relays = ProfileRelayObservation::default();
    let scope = profile_publication_scope(&node, &mut relays, now);
    let peer = fake_peer(false);
    let uid = peer.public.user_id();
    let mut published = std::collections::HashMap::new();
    profile_republish_targets(&mut published, std::slice::from_ref(&peer), 1, scope, now);
    let owner = published[&uid].owner.clone();
    for _ in 0..5 {
        finish_profile_publication(&mut published, &uid, &owner, false, now);
    }
    // The real routed-discovery owner can extend public evidence for a slow
    // authenticated response. That lifetime is not new publication input.
    node.roster.lock().unwrap().update_routed_with_latency(
        &proof,
        ip,
        &node.user_id(),
        Duration::from_millis(200),
    );
    let refreshed = profile_publication_scope(&node, &mut relays, now + Duration::from_secs(1));
    assert!(
        scope == refreshed,
        "same signed relay heartbeat must not grant retry credit"
    );
    let crossed = original_deadline + Duration::from_secs(1);
    let continuous = profile_publication_scope(&node, &mut relays, crossed);
    assert!(
        scope == continuous,
        "crossing the original lease must not invent relay revival"
    );
    assert!(
        profile_republish_targets(
            &mut published,
            std::slice::from_ref(&peer),
            1,
            continuous,
            crossed
        )
        .is_empty()
    );
    let renewed_deadline = node
        .roster
        .lock()
        .unwrap()
        .get(&relay.user_id())
        .unwrap()
        .presence_deadline(crossed)
        .unwrap();
    // Observe a later genuinely expired-to-fresh transition using the same
    // production lifetime API, without changing private tracker state or sleeping.
    let revived = renewed_deadline + Duration::from_secs(1);
    node.roster.lock().unwrap().update_routed_with_latency(
        &proof,
        ip,
        &node.user_id(),
        Duration::from_secs(1),
    );
    let recovered = profile_publication_scope(&node, &mut relays, revived);
    assert!(
        recovered != continuous,
        "actual relay revival must change admission input"
    );
    assert_eq!(
        profile_republish_targets(
            &mut published,
            std::slice::from_ref(&peer),
            1,
            recovered,
            revived
        )
        .len(),
        1
    );
    let owner = published[&uid].owner.clone();
    finish_profile_publication(&mut published, &uid, &owner, false, revived);
    let moved = Announce::new_post_office_with_account(&relay, &account, "Relay", 21214);
    node.roster.lock().unwrap().update_routed_with_latency(
        &moved,
        ip,
        &node.user_id(),
        Duration::from_secs(1),
    );
    let rerouted = profile_publication_scope(&node, &mut relays, revived);
    assert_eq!(
        profile_republish_targets(&mut published, &[peer], 1, rerouted, revived).len(),
        1,
        "a new eligible relay endpoint must bypass the unchanged recipient cooldown"
    );
    node.roster.lock().unwrap().forget(&relay.public());
    profile_publication_scope(&node, &mut relays, revived);
    assert!(
        relays.known.is_empty(),
        "removed relay observations cannot accumulate"
    );
}

#[test]
fn profile_republish_recent_failure_waits_while_new_revision_remains_prompt() {
    let peer = fake_peer(false);
    let mut published = std::collections::HashMap::new();
    let now = std::time::Instant::now();
    let scope = ProfilePublicationScope::default();
    assert_eq!(
        profile_republish_targets(&mut published, std::slice::from_ref(&peer), 1, scope, now).len(),
        1,
        "first sight must be eligible"
    );
    let owner = published[&peer.public.user_id()].owner.clone();
    prepare_profile_publication(
        published.get_mut(&peer.public.user_id()).unwrap(),
        now,
        || Ok(Some(ConversationId::new([0; 32]))),
    )
    .unwrap();
    // The network attempt lasts longer than its first retry delay. Only its
    // completion, never its start, may begin the next wait.
    let completed = now + Duration::from_secs(30);
    finish_profile_publication(
        &mut published,
        &peer.public.user_id(),
        &owner,
        false,
        completed,
    );
    assert!(
        profile_republish_targets(
            &mut published,
            std::slice::from_ref(&peer),
            1,
            scope,
            completed + Duration::from_secs(4)
        )
        .is_empty(),
        "an unchanged failed publication must wait for its retry deadline"
    );
    assert_eq!(
        profile_republish_targets(
            &mut published,
            std::slice::from_ref(&peer),
            2,
            scope,
            completed
        )
        .len(),
        1,
        "new profile input must remain prompt despite the old failure"
    );
}

#[test]
fn profile_republish_targets_fires_on_first_sight_and_on_reconnect() {
    let bob = fake_peer(false);
    let mut published = std::collections::HashMap::new();
    let now = std::time::Instant::now();
    let scope = ProfilePublicationScope::default();

    // First sight → publish to Bob.
    assert_eq!(
        profile_republish_targets(&mut published, std::slice::from_ref(&bob), 1, scope, now).len(),
        1
    );
    // Merely selecting a target cannot mark an unstarted attempt successful.
    assert_eq!(
        profile_republish_targets(&mut published, std::slice::from_ref(&bob), 1, scope, now).len(),
        1
    );
    let owner = published[&bob.public.user_id()].owner.clone();
    finish_profile_publication(&mut published, &bob.public.user_id(), &owner, true, now);
    // Still present next round → no re-publish (no log churn).
    assert!(
        profile_republish_targets(&mut published, std::slice::from_ref(&bob), 1, scope, now)
            .is_empty()
    );
    assert_eq!(
        profile_republish_targets(&mut published, std::slice::from_ref(&bob), 2, scope, now).len(),
        1,
        "a newer public profile must reach an already-present peer"
    );
    // Bob goes offline (evicted from the roster).
    assert!(profile_republish_targets(&mut published, &[], 1, scope, now).is_empty());
    assert!(
        published.is_empty(),
        "retired peers cannot accumulate scheduling state"
    );
    // Bob reconnects → published to AGAIN. The old monotonic `seen` set skipped him here,
    // so he kept showing our stale avatar until we manually re-set it.
    assert_eq!(
        profile_republish_targets(&mut published, std::slice::from_ref(&bob), 1, scope, now).len(),
        1,
        "a reconnecting peer must be re-published to"
    );

    // A post office is never a profile target.
    let mut p2 = std::collections::HashMap::new();
    assert!(
        profile_republish_targets(
            &mut p2,
            std::slice::from_ref(&fake_peer(true)),
            1,
            scope,
            now
        )
        .is_empty()
    );

    // A failing older peer cannot keep taking a bounded slot ahead of untouched
    // peers. Once both were attempted, the older failed attempt gets its turn.
    let charlie = fake_peer(false);
    let owner = published[&bob.public.user_id()].owner.clone();
    prepare_profile_publication(
        published.get_mut(&bob.public.user_id()).unwrap(),
        now,
        || Ok(Some(ConversationId::new([0; 32]))),
    )
    .unwrap();
    finish_profile_publication(&mut published, &bob.public.user_id(), &owner, false, now);
    let peers = [bob.clone(), charlie.clone()];
    assert_eq!(
        profile_republish_targets(&mut published, &peers, 1, scope, now)[0].public,
        charlie.public
    );
    let owner = published[&charlie.public.user_id()].owner.clone();
    prepare_profile_publication(
        published.get_mut(&charlie.public.user_id()).unwrap(),
        now + Duration::from_millis(1),
        || Ok(Some(ConversationId::new([1; 32]))),
    )
    .unwrap();
    finish_profile_publication(
        &mut published,
        &charlie.public.user_id(),
        &owner,
        false,
        now + Duration::from_millis(1),
    );
    assert_eq!(
        profile_republish_targets(
            &mut published,
            &peers,
            1,
            scope,
            now + Duration::from_secs(6)
        )[0]
        .public,
        bob.public
    );
}

#[test]
fn profile_republish_preparation_failures_back_off_and_recover() {
    let peer = fake_peer(false);
    let uid = peer.public.user_id();
    let scope = ProfilePublicationScope::default();
    let mut published = std::collections::HashMap::new();
    let mut now = std::time::Instant::now();
    // Preparation errors and unavailable authorization must both wait. Exercise
    // the same preparation/result owners as the runtime, without network I/O.
    for (index, seconds) in [5, 10, 20, 40, 80, 160, 300, 300].into_iter().enumerate() {
        assert_eq!(
            profile_republish_targets(&mut published, std::slice::from_ref(&peer), 1, scope, now)
                .len(),
            1
        );
        let owner = published[&uid].owner.clone();
        let result = prepare_profile_publication(published.get_mut(&uid).unwrap(), now, || {
            if index % 2 == 0 {
                Err(NodeError::InvalidInput(
                    "fixture preparation failure".into(),
                ))
            } else {
                Ok(None)
            }
        });
        assert!(!matches!(result, Ok(Some(_))));
        // Completion occurs after preparation starts, preserving its whole wait.
        let completed = now + Duration::from_secs(2);
        finish_profile_publication(&mut published, &uid, &owner, false, completed);
        assert!(
            profile_republish_targets(
                &mut published,
                std::slice::from_ref(&peer),
                1,
                scope,
                completed + Duration::from_secs(seconds) - Duration::from_millis(1)
            )
            .is_empty()
        );
        now = completed + Duration::from_secs(seconds);
    }
    assert_eq!(
        profile_republish_targets(&mut published, std::slice::from_ref(&peer), 1, scope, now).len(),
        1
    );
    let owner = published[&uid].owner.clone();
    let conversation = ConversationId::new([3; 32]);
    assert_eq!(
        prepare_profile_publication(published.get_mut(&uid).unwrap(), now, || Ok(Some(
            conversation
        )))
        .unwrap(),
        Some(conversation)
    );
    assert_eq!(
        prepare_profile_publication(published.get_mut(&uid).unwrap(), now, || panic!(
            "retained accepted preparation must be reused"
        ))
        .unwrap(),
        Some(conversation)
    );
    finish_profile_publication(&mut published, &uid, &owner, true, now);
    assert!(
        profile_republish_targets(
            &mut published,
            &[peer],
            1,
            scope,
            now + Duration::from_secs(1000)
        )
        .is_empty()
    );
}

#[test]
fn profile_republish_changed_inputs_reject_stale_completion() {
    let now = std::time::Instant::now();
    for change in 0..8 {
        let peer = fake_peer(false);
        let uid = peer.public.user_id();
        let mut published = std::collections::HashMap::new();
        let mut scope = ProfilePublicationScope::default();
        profile_republish_targets(&mut published, std::slice::from_ref(&peer), 1, scope, now);
        let old = published[&uid].owner.clone();
        finish_profile_publication(&mut published, &uid, &old, false, now);
        let mut replacement = peer.clone();
        let mut revision = 1;
        match change {
            0 => revision = 2,
            1 => replacement.public.x25519_pub[0] ^= 1,
            2 => replacement.addr.set_port(2),
            3 => scope.privacy += 1,
            4 => scope.network += 1,
            5 => scope.bridge += 1,
            6 => scope.device_policy += 1,
            _ => {
                profile_republish_targets(&mut published, &[], 1, scope, now);
            }
        }
        assert_eq!(
            profile_republish_targets(
                &mut published,
                std::slice::from_ref(&replacement),
                revision,
                scope,
                now
            )
            .len(),
            1,
            "changed target/source/policy or reappearance must bypass old cooldown"
        );
        let current = published[&uid].owner.clone();
        finish_profile_publication(&mut published, &uid, &old, true, now);
        assert_eq!(
            profile_republish_targets(
                &mut published,
                std::slice::from_ref(&replacement),
                revision,
                scope,
                now
            )
            .len(),
            1,
            "an old successful completion cannot settle replacement work"
        );
        finish_profile_publication(&mut published, &uid, &current, true, now);
        finish_profile_publication(&mut published, &uid, &old, false, now);
        assert!(
            profile_republish_targets(&mut published, &[replacement], revision, scope, now)
                .is_empty(),
            "an old failure cannot restart completed replacement work"
        );
    }
}

#[test]
fn profile_republish_heartbeats_preserve_backoff_but_revival_is_prompt() {
    use crate::discovery::roster::{PUBLIC_PRESENCE_TTL, PeerEndpoint, RouteSource};
    let mut peer = fake_peer(false);
    let uid = peer.public.user_id();
    let scope = ProfilePublicationScope::default();
    let now = std::time::Instant::now();
    peer.routes.push(PeerEndpoint {
        addr: peer.addr,
        source: RouteSource::Lan,
        last_seen: now,
        last_authenticated: None,
        presence_expires_at: None,
        presence_origin_expires_at: None,
        origin_hops: None,
        registration_origin: None,
    });
    let mut published = std::collections::HashMap::new();
    profile_republish_targets(&mut published, std::slice::from_ref(&peer), 1, scope, now);
    let owner = published[&uid].owner.clone();
    // Several immediate failures set a cooldown longer than public presence.
    // Deadline comparisons use the real scheduler's injected observation time.
    for _ in 0..5 {
        finish_profile_publication(&mut published, &uid, &owner, false, now);
    }
    let refresh = now + PUBLIC_PRESENCE_TTL / 2;
    peer.routes[0].last_seen = refresh;
    peer.last_seen = refresh;
    peer.name = "heartbeat display metadata".into();
    assert!(
        profile_republish_targets(
            &mut published,
            std::slice::from_ref(&peer),
            1,
            scope,
            refresh
        )
        .is_empty()
    );
    let crossed = now + PUBLIC_PRESENCE_TTL + Duration::from_secs(1);
    peer.routes[0].last_seen = crossed;
    assert!(
        profile_republish_targets(
            &mut published,
            std::slice::from_ref(&peer),
            1,
            scope,
            crossed
        )
        .is_empty(),
        "steady freshness must not manufacture reconnect credit at the original deadline"
    );
    let revived = crossed + PUBLIC_PRESENCE_TTL + Duration::from_secs(1);
    peer.routes[0].last_seen = revived;
    assert_eq!(
        profile_republish_targets(
            &mut published,
            std::slice::from_ref(&peer),
            1,
            scope,
            revived
        )
        .len(),
        1,
        "genuine same-endpoint expiry then revival must be prompt"
    );
}

#[tokio::test]
async fn start_opens_a_node_and_exposes_its_identity() {
    let dir = tempfile::tempdir().unwrap();
    // An uncommon discovery port so the test doesn't touch the real one.
    let dp = 47990;
    let runtime = NodeRuntime::start_configured(
        RuntimeConfig {
            base_dir: dir.path(),
            account_id: "alice-user-id",
            display_name: "Alice",
            password: "pw",
            discovery_port: dp,
            post_office_enabled: false,
            candidate_source: None,
        },
        RuntimeEvents {
            on_dm: Box::new(|_dm| {}),
            on_channel: Box::new(|_ch| {}),
            on_file: Box::new(|_f| {}),
            on_profile: Box::new(|_p| {}),
            on_call_signal: Box::new(|_s| {}),
        },
    )
    .await
    .expect("runtime starts");

    // The node opened and exposes a stable fingerprint; no peers yet.
    assert_eq!(runtime.user_id().len(), 32); // user_id is 32 hex chars
    assert!(runtime.peers().is_empty());
    assert!(runtime.peer_public("nobody").is_none());
    let card = runtime
        .handle()
        .account_profile(runtime.account_id())
        .unwrap()
        .unwrap();
    assert_eq!(card.display_name, "Alice");
    assert!(card.created_at_ms.is_some());
    assert!(card.verify());
    assert!(
        !runtime
            .handle()
            .relay_allowed(&crate::identity::device::DeviceIdentity::generate().public())
    );

    // Per-user files were created on disk under accounts/<user_id>/.
    let node_dir = dir.path().join("accounts").join("alice-user-id");
    assert!(node_dir.join("identity.keystore").exists());
    assert!(node_dir.join("messages.log").exists());
    assert!(node_dir.join("sent.log").exists());

    let user_id = runtime.user_id().to_owned();
    let account_id = runtime.account_id().to_owned();
    let host_account_id = runtime.host_account_id().map(str::to_owned);
    runtime.stop().await;

    // Reopening after joined shutdown reloads the SAME identity (persistent).
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
        |_s| {},
    )
    .await
    .expect("runtime reopens");
    assert_eq!(user_id, runtime2.user_id());

    // The cryptographic account is created on disk and stable across reopen.
    assert_eq!(account_id.len(), 32); // account_id is 32 hex chars
    assert_eq!(host_account_id.as_deref(), Some("alice-user-id"));
    assert!(node_dir.join("account.keystore").exists());
    assert_eq!(account_id, runtime2.account_id());
    runtime2.stop().await;
}

#[tokio::test]
async fn existing_account_without_creation_evidence_stays_unknown() {
    use crate::storage::encryption::{EncryptionKey, encrypt_data, generate_salt};

    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("accounts").join("legacy");
    std::fs::create_dir_all(&profile).unwrap();
    let account = crate::identity::account::Account::generate();
    // An actual pre-metadata keystore contains only the encrypted secret.
    // Saving a newly generated account now correctly persists its creation date.
    let salt = generate_salt();
    let key = EncryptionKey::from_password("pw", &salt).unwrap();
    let (ciphertext, nonce) = encrypt_data(&account.secret_bytes(), &key).unwrap();
    let mut legacy = salt.to_vec();
    legacy.extend_from_slice(&nonce);
    legacy.extend_from_slice(&ciphertext);
    let keystore = profile.join("account.keystore");
    std::fs::write(&keystore, legacy).unwrap();
    assert_eq!(
        crate::identity::account_keystore::load(&keystore, "pw")
            .unwrap()
            .created_at_ms(),
        None
    );
    let runtime = NodeRuntime::start(
        dir.path(),
        "legacy",
        "Legacy",
        "pw",
        0,
        |_| {},
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await
    .unwrap();
    assert!(
        runtime
            .handle()
            .account_profile(&account.account_id())
            .unwrap()
            .unwrap()
            .created_at_ms
            .is_none()
    );
    runtime.stop().await;
}

#[tokio::test]
async fn set_display_name_updates_advertised_name_and_own_record() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = NodeRuntime::start(
        dir.path(),
        "alice-user-id",
        "Alice",
        "pw",
        47991,
        |_dm| {},
        |_ch| {},
        |_f| {},
        |_p| {},
        |_s| {},
    )
    .await
    .expect("runtime starts");

    assert_eq!(runtime.display_name(), "Alice");
    let uid = runtime.user_id().to_string();

    // Rename to a permissive (spaces + emoji) display name.
    runtime.set_display_name("Alice Renamed 🐻");
    assert_eq!(
        runtime
            .handle()
            .account_profile(runtime.account_id())
            .unwrap()
            .unwrap()
            .display_name,
        "Alice Renamed 🐻"
    );

    // The advertised name and our own durable record both reflect the change; the
    // identity (user_id/account_id) is untouched.
    assert_eq!(runtime.display_name(), "Alice Renamed 🐻");
    assert_eq!(
        runtime.display_name_for(&uid).as_deref(),
        Some("Alice Renamed 🐻")
    );
    assert_eq!(runtime.user_id(), uid);
}

#[tokio::test]
async fn startup_network_options_precede_discovery_and_delivery_launch() {
    let root = tempfile::tempdir().unwrap();
    let reserve = crate::transport::net::discovery_socket(0).unwrap();
    let port = reserve.local_addr().unwrap().port();
    drop(reserve);
    let runtime = NodeRuntime::start_configured_guarded_with_network_cost_observed(
        RuntimeConfig {
            base_dir: root.path(),
            account_id: "network-options",
            display_name: "Scoped",
            password: "pw",
            discovery_port: port,
            post_office_enabled: true,
            candidate_source: None,
        },
        RuntimeEvents {
            on_dm: Box::new(|_| {}),
            on_channel: Box::new(|_| {}),
            on_file: Box::new(|_| {}),
            on_profile: Box::new(|_| {}),
            on_call_signal: Box::new(|_| {}),
        },
        RuntimeNetworkOptions {
            post_office_send_enabled: false,
            post_office_receive_enabled: true,
            physical_lan_only: true,
        },
        crate::node::NetworkCost::Unknown,
        |_| Ok(()),
        |launch| {
            let runtime = launch("Scoped")?;
            let node = runtime.handle();
            let relay = crate::identity::device::DeviceIdentity::generate().public();
            assert!(node.physical_lan_only());
            assert!(
                node.group_file_sharing_status().paused,
                "cost protection precedes participant launch"
            );
            assert!(!node.relay_send_allowed(&relay));
            assert!(node.relay_receive_allowed(&relay));
            assert!(!node.bridge_directory_allowed());
            Ok(runtime)
        },
    )
    .await
    .unwrap();
    assert!(
        !runtime.participant.visibility.routed_allowed().await,
        "persisted scope initializes shared discovery gate before background probes"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn contact_nickname_survives_a_stale_signed_card_name_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let start = || {
        NodeRuntime::start(
            root.path(),
            "owner",
            "Owner nickname",
            "pw",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        )
    };
    let runtime = start().await.unwrap();
    let device = crate::identity::device::DeviceIdentity::generate();
    let account = crate::identity::account::Account::generate();
    let stale_card = super::super::trust_card::TrustCard::new(
        &account,
        1,
        "steven".into(),
        "Existing personal signature".into(),
        None,
        None,
    );
    runtime.handle().receive_trust_card(&stale_card).unwrap();
    let nickname = crate::discovery::Announce::new_with_account(&device, &account, "Duncan", 1);
    runtime.roster.lock().unwrap().update(
        &nickname,
        "127.0.0.2".parse().unwrap(),
        runtime.user_id(),
    );
    runtime.handle().cached_peer_snapshot().unwrap();
    assert_eq!(
        runtime.known_account_names().unwrap()[&account.account_id()][0],
        "Duncan"
    );
    // Reading a profile must neither rename the contact nor mutate its claims.
    assert_eq!(
        runtime.handle().trust_card(&account.account_id()).unwrap(),
        Some(stale_card.clone())
    );
    runtime.roster.lock().unwrap().evict_stale(Duration::ZERO);
    assert_eq!(
        runtime.known_account_names().unwrap()[&account.account_id()][0],
        "Duncan"
    );
    runtime.stop().await;
    let reopened = start().await.unwrap();
    assert_eq!(
        reopened.known_account_names().unwrap()[&account.account_id()][0],
        "Duncan"
    );
    assert_eq!(
        reopened.handle().trust_card(&account.account_id()).unwrap(),
        Some(stale_card)
    );
    reopened.stop().await;
}

#[tokio::test]
async fn offline_account_names_survive_roster_expiry_and_restart_without_presence() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = NodeRuntime::start(
        dir.path(),
        "owner",
        "Owner",
        "pw",
        0,
        |_| {},
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await
    .unwrap();
    let device = crate::identity::device::DeviceIdentity::generate();
    let account = crate::identity::account::Account::generate();
    let older = crate::identity::device::DeviceIdentity::generate();
    let old_proof = crate::discovery::Announce::new_with_account(&older, &account, "Old name", 1);
    runtime.roster.lock().unwrap().update(
        &old_proof,
        "127.0.0.2".parse().unwrap(),
        runtime.user_id(),
    );
    runtime.handle().cached_peer_snapshot().unwrap();
    assert_eq!(
        runtime.known_account_names().unwrap()[&account.account_id()][0],
        "Old name"
    );
    let proof = crate::discovery::Announce::new_with_account(&device, &account, "Messi10", 1);
    runtime
        .roster
        .lock()
        .unwrap()
        .update(&proof, "127.0.0.1".parse().unwrap(), runtime.user_id());
    runtime.handle().cached_peer_snapshot().unwrap();
    assert_eq!(
        runtime.known_account_names().unwrap()[&account.account_id()][0],
        "Messi10"
    );
    runtime.roster.lock().unwrap().update(
        &old_proof,
        "127.0.0.2".parse().unwrap(),
        runtime.user_id(),
    );
    runtime.handle().cached_peer_snapshot().unwrap();
    assert_eq!(
        runtime.known_account_names().unwrap()[&account.account_id()][0],
        "Messi10"
    );
    runtime.roster.lock().unwrap().evict_stale(Duration::ZERO);
    assert!(runtime.peers().is_empty());
    assert_eq!(
        runtime.known_account_names().unwrap()[&account.account_id()][0],
        "Messi10"
    );
    runtime.stop().await;
    let reopened = NodeRuntime::start(
        dir.path(),
        "owner",
        "Owner",
        "pw",
        0,
        |_| {},
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(
        reopened.known_account_names().unwrap()[&account.account_id()][0],
        "Messi10"
    );
    assert!(
        reopened
            .peers()
            .iter()
            .all(|peer| peer.presence_evidence(std::time::Instant::now()).is_empty())
    );
    reopened.stop().await;
}

#[tokio::test]
async fn account_nickname_preserves_bio_and_wins_over_host_name_on_restart() {
    let root = tempfile::tempdir().unwrap();
    let runtime = NodeRuntime::start(
        root.path(),
        "owner",
        "Original",
        "pw",
        0,
        |_| {},
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await
    .unwrap();
    runtime
        .handle()
        .save_account_profile_if("Original".into(), "Personal signature".into(), |commit| {
            commit()
        })
        .await
        .unwrap();
    runtime
        .handle()
        .set_avatar(Some(b"avatar".to_vec()))
        .await
        .unwrap();
    let old = runtime
        .handle()
        .account_profile(runtime.account_id())
        .unwrap()
        .unwrap();
    let joining_dir = root.path().join("accounts/joining");
    crate::identity::account_keystore::save(
        &joining_dir.join("account.keystore"),
        "pw",
        &runtime.node.account,
    )
    .unwrap();
    // This fixture models an existing account on a sibling device. Seed its
    // signed card as well as its key: a key-only clone is a fresh single-device
    // account and legitimately creates a newer card from the host name.
    crate::node::account_profile_data::AccountProfileStore::open(
        &joining_dir.join("account-profiles.log"),
        "pw",
    )
    .unwrap()
    .record(old.clone())
    .unwrap();
    let joining = NodeRuntime::start(
        root.path(),
        "joining",
        "Joining local nickname",
        "pw",
        0,
        |_| {},
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(
        joining
            .handle()
            .account_profile(joining.account_id())
            .unwrap(),
        Some(old.clone()),
        "the sibling starts with the existing signed account profile"
    );
    for (receiver, sender) in [(&runtime, &joining), (&joining, &runtime)] {
        receiver
            .node
            .account_devices
            .lock()
            .unwrap()
            .insert(crate::node::AccountDevice {
                public: sender.node.identity.public(),
                certificate: runtime
                    .node
                    .account
                    .certify(&sender.node.identity.public().ed25519_pub),
            })
            .unwrap();
        receiver.roster.lock().unwrap().update(
            &sender
                .node
                .signed_announce(sender.display_name(), sender.listen_tcp_port()),
            "127.0.0.1".parse().unwrap(),
            receiver.user_id(),
        );
    }
    runtime
        .handle()
        .set_account_display_name("Canonical account")
        .unwrap();
    let card = runtime
        .handle()
        .account_profile(runtime.account_id())
        .unwrap()
        .unwrap();
    assert_eq!(card.bio, old.bio);
    assert_eq!(card.avatar, old.avatar);
    assert_eq!(card.created_at_ms, old.created_at_ms);
    assert!(card.revision > old.revision && card.verify());
    tokio::time::timeout(Duration::from_secs(10), async {
        while joining
            .handle()
            .account_profile(joining.account_id())
            .unwrap()
            .as_ref()
            != Some(&card)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("runtime automatically republishes the nickname edit to its sibling");
    joining
        .handle()
        .set_account_display_name("Edited on joined device")
        .unwrap();
    let shared = joining
        .handle()
        .account_profile(joining.account_id())
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while runtime
            .handle()
            .account_profile(runtime.account_id())
            .unwrap()
            .as_ref()
            != Some(&shared)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the kept device receives the joining device's later nickname edit");
    assert_eq!(shared.bio, "Personal signature");
    assert_eq!(shared.avatar, Some(b"avatar".to_vec()));
    joining.stop().await;
    runtime.stop().await;
    let reopened = NodeRuntime::start(
        root.path(),
        "owner",
        "Joining local nickname",
        "pw",
        0,
        |_| {},
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(reopened.display_name(), "Edited on joined device");
    assert_eq!(
        reopened
            .handle()
            .account_profile(reopened.account_id())
            .unwrap()
            .unwrap()
            .bio,
        "Personal signature"
    );
    reopened.stop().await;
}

#[tokio::test]
async fn saved_messages_sync_between_linked_runtimes_and_survive_restart() {
    let clock = std::time::Instant::now();
    let root = tempfile::tempdir().unwrap();
    let start = |profile: &'static str| {
        NodeRuntime::start(
            root.path(),
            profile,
            profile,
            "pw",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        )
    };
    let source = start("saved-source").await.unwrap();
    crate::identity::account_keystore::save(
        &root.path().join("accounts/saved-receiver/account.keystore"),
        "pw",
        &source.node.account,
    )
    .unwrap();
    source
        .send_to_account(source.account_id(), b"saved before sibling started", None)
        .await
        .unwrap();
    let offline_accepted = clock.elapsed();
    let receiver = start("saved-receiver").await.unwrap();
    for runtime in [&source, &receiver] {
        runtime
            .handle()
            .set_device_sync_category(crate::node::SyncCategory::History, true)
            .unwrap();
        runtime
            .handle()
            .set_device_sync_category(crate::node::SyncCategory::Profile, true)
            .unwrap();
    }
    for (target, peer) in [(&source, &receiver), (&receiver, &source)] {
        target
            .node
            .account_devices
            .lock()
            .unwrap()
            .insert(crate::node::AccountDevice {
                public: peer.node.identity.public(),
                certificate: source
                    .node
                    .account
                    .certify(&peer.node.identity.public().ed25519_pub),
            })
            .unwrap();
        target.roster.lock().unwrap().update(
            &peer
                .node
                .signed_announce(peer.display_name(), peer.listen_tcp_port()),
            "127.0.0.1".parse().unwrap(),
            target.user_id(),
        );
    }
    let routes_ready = clock.elapsed();
    tokio::time::timeout(Duration::from_secs(12), async {
        while !receiver
            .account_history(receiver.account_id(), 20)
            .unwrap()
            .iter()
            .any(|row| row.text == b"saved before sibling started")
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Saved Messages created offline is replayed to the linked runtime");
    eprintln!(
        "saved-message latency offline: accepted_at={offline_accepted:?}, routes_ready_at={routes_ready:?}, receiver_history_at={:?}",
        clock.elapsed()
    );
    let live_started = clock.elapsed();
    source
        .handle()
        .enqueue_to_account(source.account_id(), b"saved on first device", None)
        .await
        .unwrap();
    let live_accepted = clock.elapsed();
    tokio::time::timeout(Duration::from_secs(12), async {
        while !receiver
            .account_history(receiver.account_id(), 20)
            .unwrap()
            .iter()
            .any(|row| row.text == b"saved on first device")
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Saved Messages automatically reaches the linked runtime");
    eprintln!(
        "saved-message latency live: enqueue_started_at={live_started:?}, accepted_at={live_accepted:?}, receiver_history_at={:?}",
        clock.elapsed()
    );
    let reverse_started = clock.elapsed();
    receiver
        .handle()
        .enqueue_to_account(receiver.account_id(), b"saved on second device", None)
        .await
        .unwrap();
    let reverse_accepted = clock.elapsed();
    tokio::time::timeout(Duration::from_secs(12), async {
        while source
            .account_history(source.account_id(), 20)
            .unwrap()
            .len()
            != 3
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("reverse Saved Messages sync completes");
    eprintln!(
        "saved-message latency reverse: enqueue_started_at={reverse_started:?}, accepted_at={reverse_accepted:?}, receiver_history_at={:?}",
        clock.elapsed()
    );
    let expected: std::collections::HashSet<_> = source
        .account_history(source.account_id(), 20)
        .unwrap()
        .into_iter()
        .map(|row| {
            assert!(row.from_me);
            row.id
        })
        .collect();
    receiver.stop().await;
    let restarted = start("saved-receiver").await.unwrap();
    let actual: std::collections::HashSet<_> = restarted
        .account_history(restarted.account_id(), 20)
        .unwrap()
        .into_iter()
        .map(|row| {
            assert!(row.from_me);
            row.id
        })
        .collect();
    assert_eq!(actual, expected);
    source.stop().await;
    restarted.stop().await;
}

#[tokio::test]
async fn historical_account_name_does_not_follow_a_retained_devices_new_account() {
    let dir = tempfile::tempdir().unwrap();
    let start = || {
        NodeRuntime::start(
            dir.path(),
            "owner",
            "Owner",
            "pw",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        )
    };
    let runtime = start().await.unwrap();
    let device = crate::identity::device::DeviceIdentity::generate();
    let old_account = crate::identity::account::Account::generate();
    let new_account = crate::identity::account::Account::generate();
    let old =
        crate::discovery::Announce::new_with_account(&device, &old_account, "Old principal", 1);
    runtime
        .handle()
        .privacy
        .state
        .write()
        .unwrap()
        .as_mut()
        .unwrap()
        .proofs
        .record(&old)
        .unwrap();
    let new =
        crate::discovery::Announce::new_with_account(&device, &new_account, "New principal", 1);
    runtime
        .roster
        .lock()
        .unwrap()
        .update(&new, "127.0.0.2".parse().unwrap(), runtime.user_id());
    assert_eq!(
        runtime.known_account_names().unwrap()[&old_account.account_id()][0],
        "Old principal"
    );
    runtime.stop().await;
    let reopened = start().await.unwrap();
    assert_eq!(
        reopened.known_account_names().unwrap()[&old_account.account_id()][0],
        "Old principal",
        "an unscoped durable device nickname cannot rename its old account after restart"
    );
    reopened.stop().await;
}

async fn transition_runtime(root: &std::path::Path, owner: &str) -> NodeRuntime {
    NodeRuntime::start(
        root,
        owner,
        owner,
        "pw",
        0,
        |_| {},
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await
    .unwrap()
}

async fn transition_pair(source: &NodeRuntime, target: &NodeRuntime) {
    for (receiver, sender) in [(source, target), (target, source)] {
        receiver.roster.lock().unwrap().update(
            &sender
                .node
                .signed_announce(sender.display_name(), sender.listen_tcp_port()),
            "127.0.0.1".parse().unwrap(),
            receiver.user_id(),
        );
        receiver.node.cached_peer_snapshot_async().await.unwrap();
    }
    source.initiate_contact(target.account_id()).await.unwrap();
    target.initiate_contact(source.account_id()).await.unwrap();
    source
        .node
        .publish_account_access_if(b"test source credentials".to_vec(), |commit| commit())
        .unwrap();
    target
        .node
        .set_device_sync_category(crate::node::SyncCategory::History, false)
        .unwrap();
}

#[tokio::test]
async fn authenticated_link_stages_transition_until_loaded_keystore_restart() {
    let root = tempfile::tempdir().unwrap();
    let source = transition_runtime(root.path(), "transition-source").await;
    let target = transition_runtime(root.path(), "transition-target").await;
    transition_pair(&source, &target).await;
    let previous = target.account_id().to_owned();
    let current = source.account_id().to_owned();
    let public = target.node.identity.public();
    let code = source.start_linking();
    assert_eq!(
        target
            .link_device_with_account_access_if(source.user_id(), &code, |commit| commit())
            .await
            .unwrap(),
        current,
    );
    assert!(
        target.node.own_account_transitions().is_empty(),
        "the old running account must not publish the pending future binding"
    );
    target.stop().await;
    let reopened = transition_runtime(root.path(), "transition-target").await;
    assert_eq!(reopened.account_id(), current);
    let transitions = reopened.node.own_account_transitions();
    assert_eq!(
        transitions.len(),
        1,
        "a completed authenticated link must survive restart as a signed account transition"
    );
    let proof = &transitions[0];
    assert!(proof.verify());
    assert_eq!(proof.public, public);
    assert_eq!(proof.previous_account_id(), previous);
    assert_eq!(proof.current_account_id(), current);
    assert!(proof.previous_transition.is_none());
    reopened.stop().await;
    source.stop().await;
}

#[tokio::test]
async fn failed_link_keystore_save_does_not_publish_and_can_retry_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let source = transition_runtime(root.path(), "failed-transition-source").await;
    let target = transition_runtime(root.path(), "failed-transition-target").await;
    transition_pair(&source, &target).await;
    let previous = target.account_id().to_owned();
    let current = source.account_id().to_owned();
    let key = root
        .path()
        .join("accounts/failed-transition-target/account.keystore");
    let saved = key.with_extension("original-test-keystore");
    std::fs::rename(&key, &saved).unwrap();
    std::fs::create_dir(&key).unwrap();
    let code = source.start_linking();
    let failed = target
        .link_device_with_account_access_if(source.user_id(), &code, |commit| commit())
        .await;
    assert!(
        failed.is_err(),
        "a directory in place of the test keystore must reject atomic replacement"
    );
    assert!(target.node.own_account_transitions().is_empty());
    std::fs::remove_dir(&key).unwrap();
    std::fs::rename(&saved, &key).unwrap();
    target.stop().await;
    let restored = transition_runtime(root.path(), "failed-transition-target").await;
    assert_eq!(restored.account_id(), previous);
    assert!(
        restored.node.own_account_transitions().is_empty(),
        "reopening the retained old account cannot commit its failed future transition"
    );
    // The linker already durably certified this exact device on the first attempt.
    // Its retired-account retry admission, rather than a fresh old-account grant,
    // is the authorized recovery path.
    restored.roster.lock().unwrap().update(
        &source
            .node
            .signed_announce(source.display_name(), source.listen_tcp_port()),
        "127.0.0.1".parse().unwrap(),
        restored.user_id(),
    );
    restored.node.cached_peer_snapshot_async().await.unwrap();
    let retry = source.start_linking();
    assert_eq!(
        restored
            .link_device_with_account_access_if(source.user_id(), &retry, |commit| commit())
            .await
            .unwrap(),
        current
    );
    restored.stop().await;
    let adopted = transition_runtime(root.path(), "failed-transition-target").await;
    assert_eq!(adopted.account_id(), current);
    assert_eq!(
        adopted.node.own_account_transitions().len(),
        1,
        "a failed attempt must not block the next authorized adoption"
    );
    adopted.stop().await;
    source.stop().await;
}

#[tokio::test]
async fn stale_link_authorization_cannot_stage_an_account_transition() {
    let root = tempfile::tempdir().unwrap();
    let source = transition_runtime(root.path(), "denied-transition-source").await;
    let target = transition_runtime(root.path(), "denied-transition-target").await;
    transition_pair(&source, &target).await;
    let previous = target.account_id().to_owned();
    let code = source.start_linking();
    let denied = target
        .link_device_with_account_access_if(source.user_id(), &code, |_commit| {
            Err(NodeError::Authorization("stale host session".into()))
        })
        .await;
    assert!(denied.is_err());
    assert!(target.node.own_account_transitions().is_empty());
    target.stop().await;
    let reopened = transition_runtime(root.path(), "denied-transition-target").await;
    assert_eq!(reopened.account_id(), previous);
    assert!(reopened.node.own_account_transitions().is_empty());
    reopened.stop().await;
    source.stop().await;
}

#[tokio::test]
async fn security_rekey_does_not_publish_linking_account_continuity() {
    let root = tempfile::tempdir().unwrap();
    let runtime = transition_runtime(root.path(), "transition-rekey").await;
    let previous = runtime.account_id().to_owned();
    let current = runtime.rekey_account().unwrap();
    assert_ne!(current, previous);
    runtime.stop().await;
    let reopened = transition_runtime(root.path(), "transition-rekey").await;
    assert_eq!(reopened.account_id(), current);
    assert!(
        reopened.node.own_account_transitions().is_empty(),
        "a compromised-account escape must not automatically carry the old account's trust or groups"
    );
    reopened.stop().await;
}
