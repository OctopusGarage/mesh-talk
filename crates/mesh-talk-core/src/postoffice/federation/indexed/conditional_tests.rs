//! Resource regressions exercise real encrypted federation, not a scheduler mock.
use super::tests::{office, opaque_event, prepare};
use super::*;
use crate::eventlog::sync::network::NetworkSyncStore;

#[tokio::test]
async fn unchanged_populated_federation_refresh_preserves_presence_without_database_writes() {
    let (_a_root, a) = office(DeviceIdentity::generate());
    let (_b_root, b) = office(DeviceIdentity::generate());
    let author = DeviceIdentity::generate();
    let arrived = unix_now();
    for _ in 0..526 {
        let receiver = DeviceIdentity::generate();
        let event = opaque_event(1, &author, &receiver);
        a.state.record(&event, arrived, None).unwrap();
        b.state.record(&event, arrived, None).unwrap();
    }
    prepare(&a);
    prepare(&b);
    let identity = DeviceIdentity::generate();
    let server_identity = DeviceIdentity::generate();
    let expected = server_identity.public();
    let directory = FederationDirectory::new();
    let remote_directory = FederationDirectory::new();
    let account = crate::identity::account::Account::generate();
    let member = DeviceIdentity::generate();
    let register = |name: &str, seen: Instant| {
        let proof = crate::discovery::Announce::new_with_account(&member, &account, name, 47475);
        directory
            .0
            .lock()
            .unwrap()
            .register(&member.public(), Some(&proof), true, seen)
            .unwrap();
    };
    register("before", Instant::now() - Duration::from_secs(2));
    let endpoint = IndexedRelayEndpoint::new(a.clone(), Some(directory.clone()), None);
    let remote = IndexedRelayEndpoint::new(
        b.clone(),
        Some(remote_directory.clone()),
        Some(IndexedRelayTrust::Pinned(Arc::new(HashSet::from([
            identity.user_id(),
        ])))),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (completed, mut completion) = tokio::sync::mpsc::channel(1);
    let server = tokio::spawn(async move {
        for _ in 0..3 {
            let channel =
                SecureChannel::accept(listener.accept().await.unwrap().0, &server_identity)
                    .await
                    .unwrap();
            remote
                .serve_connection_checked(channel, None)
                .await
                .unwrap();
            completed.send(()).await.unwrap();
        }
    });
    let mutations = |office: &IndexedPostOffice| {
        office
            .state
            .profile()
            .database()
            .read(|c| Ok(c.total_changes()))
            .unwrap()
    };
    let cleanup = |office: &IndexedPostOffice| {
        while crate::postoffice::storage::federation::cleanup_step(
            office.state.profile(),
            unix_now(),
        )
        .unwrap()
        {}
    };
    let mut observations = Vec::new();
    for turn in 0..3 {
        if turn == 1 {
            register("renewed", Instant::now());
        }
        if let Err(error) = endpoint
            .sync_with_relay_checked(addr, &identity, &expected)
            .await
        {
            server.abort();
            let _ = server.await;
            panic!("real authenticated unchanged refresh failed: {error}");
        }
        completion.recv().await.unwrap();
        cleanup(&a);
        cleanup(&b);
        observations.push((mutations(&a), mutations(&b)));
    }
    server.await.unwrap();
    assert_eq!(
        observations[1], observations[0],
        "unchanged refresh must not allocate/stage/delete SQL session inventories"
    );
    assert_eq!(
        observations[2], observations[0],
        "repeated unchanged refresh must remain write-free"
    );
    assert!(
        remote_directory
            .0
            .lock()
            .unwrap()
            .export_federated(Instant::now(), unix_now())
            .iter()
            .any(|p| p.announce.user_id == member.user_id() && p.announce.name == "renewed"),
        "write-free refresh must still propagate renewed participant presence"
    );
}

pub(super) struct PairFixture {
    _roots: [tempfile::TempDir; 2],
    pub(super) local: IndexedRelayEndpoint,
    pub(super) remote: IndexedRelayEndpoint,
    pub(super) client: DeviceIdentity,
    pub(super) server: DeviceIdentity,
}
pub(super) fn pair_fixture() -> PairFixture {
    let (ar, a) = office(DeviceIdentity::generate());
    let (br, b) = office(DeviceIdentity::generate());
    let event = opaque_event(1, &DeviceIdentity::generate(), &DeviceIdentity::generate());
    let now = unix_now();
    a.state.record(&event, now, None).unwrap();
    b.state.record(&event, now, None).unwrap();
    prepare(&a);
    prepare(&b);
    let client = DeviceIdentity::generate();
    let server = DeviceIdentity::generate();
    PairFixture {
        _roots: [ar, br],
        local: IndexedRelayEndpoint::new(a, None, None),
        remote: IndexedRelayEndpoint::new(
            b,
            None,
            Some(IndexedRelayTrust::Pinned(Arc::new(HashSet::from([
                client.user_id()
            ])))),
        ),
        client,
        server,
    }
}
pub(super) async fn pair_round(pair: &PairFixture) -> Result<usize, IndexedRelayError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let remote = pair.remote.clone();
    let (ed, x) = pair.server.secret_bytes();
    let server = tokio::spawn(async move {
        let identity = DeviceIdentity::from_secret_bytes(ed, x);
        let channel = SecureChannel::accept(listener.accept().await.unwrap().0, &identity)
            .await
            .unwrap();
        remote.serve_connection_checked(channel, None).await
    });
    let result = pair
        .local
        .sync_with_relay_checked(addr, &pair.client, &pair.server.public())
        .await;
    if result.is_err() {
        server.abort();
    }
    let remote_result = server.await;
    let count = result?;
    remote_result.unwrap()?;
    Ok(count)
}
fn changes(endpoint: &IndexedRelayEndpoint) -> u64 {
    endpoint
        .office
        .state
        .profile()
        .database()
        .read(|c| Ok(c.total_changes()))
        .unwrap()
}
#[tokio::test]
async fn conditional_proof_does_not_hide_new_mail_and_rewarms_after_convergence() {
    let pair = pair_fixture();
    assert_eq!(pair_round(&pair).await.unwrap(), 0);
    let before = (changes(&pair.local), changes(&pair.remote));
    assert_eq!(pair_round(&pair).await.unwrap(), 0);
    assert_eq!((changes(&pair.local), changes(&pair.remote)), before);
    let event = opaque_event(1, &DeviceIdentity::generate(), &DeviceIdentity::generate());
    pair.local
        .office
        .state
        .record(&event, unix_now(), None)
        .unwrap();
    prepare(&pair.local.office);
    assert_eq!(pair_round(&pair).await.unwrap(), 1);
    assert!(
        pair.remote
            .office
            .durable_have(&event.conversation_id, &event.id)
            .unwrap()
    );
    prepare(&pair.remote.office);
    // An exchange that moved data never establishes an unchanged proof.
    let before_warm = changes(&pair.local);
    assert_eq!(pair_round(&pair).await.unwrap(), 0);
    assert!(changes(&pair.local) > before_warm);
    let warmed = (changes(&pair.local), changes(&pair.remote));
    assert_eq!(pair_round(&pair).await.unwrap(), 0);
    assert_eq!((changes(&pair.local), changes(&pair.remote)), warmed);
}
#[tokio::test]
async fn restarted_endpoint_cannot_reuse_a_previous_runtime_proof() {
    let mut pair = pair_fixture();
    pair_round(&pair).await.unwrap();
    let before = changes(&pair.local);
    pair_round(&pair).await.unwrap();
    assert_eq!(changes(&pair.local), before);
    pair.local = IndexedRelayEndpoint::new(pair.local.office.clone(), None, None);
    pair_round(&pair).await.unwrap();
    assert!(
        changes(&pair.local) > before,
        "a fresh endpoint must first prove its new runtime snapshot"
    );
    let warmed = changes(&pair.local);
    pair_round(&pair).await.unwrap();
    assert_eq!(changes(&pair.local), warmed);
}
#[tokio::test]
async fn malformed_conditional_offer_never_downgrades_or_caches_unsupported() {
    let pair = pair_fixture();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (ed, x) = pair.server.secret_bytes();
    let server = tokio::spawn(async move {
        let identity = DeviceIdentity::from_secret_bytes(ed, x);
        let mut channel = SecureChannel::accept(listener.accept().await.unwrap().0, &identity)
            .await
            .unwrap();
        assert_eq!(channel.recv().await.unwrap(), REQUEST_V5);
        send_frame(&mut channel, conditional::SUPPORTED)
            .await
            .unwrap();
        let _offer = channel.recv().await.unwrap();
        send_frame(&mut channel, b"MTFI5S\x02").await.unwrap();
    });
    let result = pair
        .local
        .sync_with_relay_checked(addr, &pair.client, &pair.server.public())
        .await;
    server.await.unwrap();
    assert!(matches!(result, Err(IndexedRelayError::Protocol(_))));
    assert!(pair.local.conditional.probe_version(&pair.server.public()));
}
#[tokio::test]
async fn conditional_legacy_rejection_is_remembered_without_reprobing_each_refresh() {
    let root = tempfile::tempdir().unwrap();
    let (_local_root, a) = office(DeviceIdentity::generate());
    prepare(&a);
    let local = IndexedRelayEndpoint::new(a, None, None);
    let client = DeviceIdentity::generate();
    let identity = DeviceIdentity::generate();
    let expected = identity.public();
    let old = Arc::new(Mutex::new(
        crate::postoffice::PostOffice::open(
            &root.path().join("old"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap(),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut rejected = SecureChannel::accept(listener.accept().await.unwrap().0, &identity)
            .await
            .unwrap();
        assert_eq!(rejected.recv().await.unwrap(), REQUEST_V5);
        drop(rejected);
        for _ in 0..2 {
            let mut channel = SecureChannel::accept(listener.accept().await.unwrap().0, &identity)
                .await
                .unwrap();
            assert_eq!(channel.recv().await.unwrap(), REQUEST_V4);
            let store = Mutex::new(
                super::super::send_inventory(&mut channel, &old, None, REQUEST_V4)
                    .await
                    .unwrap(),
            );
            while matches!(
                crate::node::session::serve_one(&mut channel, &store)
                    .await
                    .unwrap(),
                Served::Handled(_)
            ) {}
        }
    });
    for _ in 0..2 {
        assert_eq!(
            local
                .sync_with_relay_checked(addr, &client, &expected)
                .await
                .unwrap(),
            0
        );
    }
    task.await.unwrap();
    assert!(!local.conditional.probe_version(&expected));
}

#[tokio::test]
async fn accepted_conditional_version_cannot_downgrade_after_early_storage_close() {
    let pair = pair_fixture();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (ed, x) = pair.server.secret_bytes();
    let server = tokio::spawn(async move {
        let identity = DeviceIdentity::from_secret_bytes(ed, x);
        let mut channel = SecureChannel::accept(listener.accept().await.unwrap().0, &identity)
            .await
            .unwrap();
        assert_eq!(channel.recv().await.unwrap(), REQUEST_V5);
        send_frame(&mut channel, conditional::SUPPORTED)
            .await
            .unwrap();
        let _offer = channel.recv().await.unwrap();
        // Receipt of the offer proves the client consumed version acceptance.
        // A supported peer may now fault while reading its current storage stamp.
        drop(channel);
    });
    let result = pair
        .local
        .sync_with_relay_checked(addr, &pair.client, &pair.server.public())
        .await;
    server.await.unwrap();
    assert!(matches!(
        result,
        Err(IndexedRelayError::Transport(_) | IndexedRelayError::Protocol(_))
    ));
    assert!(pair.local.conditional.probe_version(&pair.server.public()));
}

#[tokio::test]
async fn full_mailbox_batch_cannot_seed_an_unchanged_proof_or_skip_its_remainder() {
    let pair = pair_fixture();
    pair_round(&pair).await.unwrap();
    let events = (0..17)
        .map(|_| opaque_event(1, &DeviceIdentity::generate(), &DeviceIdentity::generate()))
        .collect::<Vec<_>>();
    for event in &events {
        pair.local
            .office
            .state
            .record(event, unix_now(), None)
            .unwrap();
    }
    prepare(&pair.local.office);
    assert_eq!(pair_round(&pair).await.unwrap(), MAX_MAILBOXES_PER_EXCHANGE);
    prepare(&pair.remote.office);
    assert_eq!(pair_round(&pair).await.unwrap(), 1);
    for event in &events {
        assert!(
            pair.remote
                .office
                .durable_have(&event.conversation_id, &event.id)
                .unwrap()
        );
    }
    prepare(&pair.remote.office);
    let before_warm = changes(&pair.local);
    assert_eq!(pair_round(&pair).await.unwrap(), 0);
    assert!(changes(&pair.local) > before_warm);
    let warmed = changes(&pair.local);
    assert_eq!(pair_round(&pair).await.unwrap(), 0);
    assert_eq!(changes(&pair.local), warmed);
}
