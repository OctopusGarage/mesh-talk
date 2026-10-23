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

#[tokio::test]
async fn runtime_automatically_queries_ipv6_candidate_when_ipv6_is_available() {
    let Ok(reserve) = routed_ipv6_socket(0) else {
        return;
    };
    let b_port = reserve.local_addr().unwrap().port();
    drop(reserve);
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
    let reserve = routed_ipv6_socket(0).unwrap();
    let a_port = reserve.local_addr().unwrap().port();
    drop(reserve);
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
    for failure_path in ["messages.log", "privacy.policy"] {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("accounts").join("owner");
        std::fs::create_dir_all(directory.join(failure_path)).unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        NAMES_OPEN_HOOKS.lock().unwrap().insert(
            directory.join("names.log"),
            Box::new(move || {
                entered_tx.send(()).unwrap();
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
        entered_rx.await.unwrap();
        let returned_while_writer_blocked =
            tokio::time::timeout(Duration::from_millis(150), &mut finished_rx)
                .await
                .is_ok();
        release_tx.send(()).unwrap();
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
    assert!(directory.join("messages.log").is_file());
    // Reopening immediately after error must not race a detached Node writer.
    let node = NodeRuntime::start(
        root.path(),
        "other",
        "Other",
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

#[test]
fn profile_republish_targets_fires_on_first_sight_and_on_reconnect() {
    let bob = fake_peer(false);
    let mut published = std::collections::HashMap::new();

    // First sight → publish to Bob.
    assert_eq!(
        profile_republish_targets(&mut published, std::slice::from_ref(&bob), 1).len(),
        1
    );
    // Failed attempts remain eligible; only a successful publication is cached.
    assert_eq!(
        profile_republish_targets(&mut published, std::slice::from_ref(&bob), 1).len(),
        1
    );
    published.insert(
        bob.public.user_id(),
        ProfilePublication {
            revision: 1,
            conversation: ConversationId::new([0; 32]),
            complete: true,
            last_attempt: None,
        },
    );
    // Still present next round → no re-publish (no log churn).
    assert!(profile_republish_targets(&mut published, std::slice::from_ref(&bob), 1).is_empty());
    assert_eq!(
        profile_republish_targets(&mut published, std::slice::from_ref(&bob), 2).len(),
        1,
        "a newer public profile must reach an already-present peer"
    );
    // Bob goes offline (evicted from the roster).
    assert!(profile_republish_targets(&mut published, &[], 1).is_empty());
    // Bob reconnects → published to AGAIN. The old monotonic `seen` set skipped him here,
    // so he kept showing our stale avatar until we manually re-set it.
    assert_eq!(
        profile_republish_targets(&mut published, std::slice::from_ref(&bob), 1).len(),
        1,
        "a reconnecting peer must be re-published to"
    );

    // A post office is never a profile target.
    let mut p2 = std::collections::HashMap::new();
    assert!(
        profile_republish_targets(&mut p2, std::slice::from_ref(&fake_peer(true)), 1).is_empty()
    );

    // A failing older peer cannot keep taking a bounded slot ahead of untouched
    // peers. Once both were attempted, the older failed attempt gets its turn.
    let charlie = fake_peer(false);
    let older = std::time::Instant::now();
    published.insert(
        bob.public.user_id(),
        ProfilePublication {
            revision: 1,
            conversation: ConversationId::new([0; 32]),
            complete: false,
            last_attempt: Some(older),
        },
    );
    let peers = [bob.clone(), charlie.clone()];
    assert_eq!(
        profile_republish_targets(&mut published, &peers, 1)[0].public,
        charlie.public
    );
    published.insert(
        charlie.public.user_id(),
        ProfilePublication {
            revision: 1,
            conversation: ConversationId::new([1; 32]),
            complete: false,
            last_attempt: Some(older + Duration::from_millis(1)),
        },
    );
    assert_eq!(
        profile_republish_targets(&mut published, &peers, 1)[0].public,
        bob.public
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
        |_s| {},
    )
    .await
    .expect("runtime reopens");
    assert_eq!(runtime.user_id(), runtime2.user_id());

    // The cryptographic account is created on disk and stable across reopen.
    assert_eq!(runtime.account_id().len(), 32); // account_id is 32 hex chars
    assert_eq!(runtime.host_account_id(), Some("alice-user-id"));
    assert!(node_dir.join("account.keystore").exists());
    assert_eq!(runtime.account_id(), runtime2.account_id());
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
    let runtime = NodeRuntime::start_configured_guarded_with_network(
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
        |launch| {
            let runtime = launch("Scoped");
            let node = runtime.handle();
            let relay = crate::identity::device::DeviceIdentity::generate().public();
            assert!(node.physical_lan_only());
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
        runtime.known_account_names()[&account.account_id()][0],
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
        runtime.known_account_names()[&account.account_id()][0],
        "Messi10"
    );
    runtime.roster.lock().unwrap().update(
        &old_proof,
        "127.0.0.2".parse().unwrap(),
        runtime.user_id(),
    );
    runtime.handle().cached_peer_snapshot().unwrap();
    assert_eq!(
        runtime.known_account_names()[&account.account_id()][0],
        "Messi10"
    );
    runtime.roster.lock().unwrap().evict_stale(Duration::ZERO);
    assert!(runtime.peers().is_empty());
    assert_eq!(
        runtime.known_account_names()[&account.account_id()][0],
        "Messi10"
    );
    drop(runtime);
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
        reopened.known_account_names()[&account.account_id()][0],
        "Messi10"
    );
    assert!(
        reopened
            .peers()
            .iter()
            .all(|peer| peer.presence_evidence(std::time::Instant::now()).is_empty())
    );
}
