use super::*;
use crate::{
    eventlog::sync::network::NetworkSyncStore,
    postoffice::storage::role::inventory::preparation::DigestWorker,
    storage::profile::ProfileStorageLease,
};
use tokio::net::TcpListener;
fn office(identity: DeviceIdentity) -> (tempfile::TempDir, IndexedPostOffice) {
    let root = tempfile::tempdir().unwrap();
    let office = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(root.path()).unwrap(),
        &root.path().join("relay.log"),
        "pw",
        identity,
    )
    .unwrap();
    (root, office)
}
fn opaque_event(seq: u64, author: &DeviceIdentity, receiver: &DeviceIdentity) -> Event {
    let inner = Event::new(
        author,
        ConversationId::new([42; 32]),
        seq,
        vec![],
        seq,
        seq,
        crate::eventlog::EventKind::Message,
        vec![42],
    );
    crate::postoffice::opaque::RelayMailbox::federated(author, &receiver.public())
        .unwrap()
        .wrap(&inner)
        .unwrap()
}
fn prepare(office: &IndexedPostOffice) {
    let now = unix_now();
    office.state.request_preparation_at(now).unwrap();
    let mut worker = DigestWorker::default();
    for _ in 0..10000 {
        worker.step_at(&office.state, now).unwrap();
        if office.state.preparation_ready_at(now).unwrap() {
            return;
        }
    }
    panic!("preparation failed to finish");
}
#[tokio::test]
async fn trusted_sql_relays_reconcile_actual_opaque_events_over_tcp_noise() {
    let client_identity = DeviceIdentity::generate();
    let server_identity = DeviceIdentity::generate();
    let expected = server_identity.public();
    let (_a, mut a) = office(DeviceIdentity::generate());
    let (_b, mut b) = office(DeviceIdentity::generate());
    let author = DeviceIdentity::generate();
    let recipient = DeviceIdentity::generate();
    let first = opaque_event(1, &author, &recipient);
    let second = opaque_event(2, &author, &recipient);
    a.ingest(first.clone()).unwrap();
    b.ingest(second.clone()).unwrap();
    prepare(&a);
    prepare(&b);
    let endpoint = IndexedRelayEndpoint::new(a.clone(), None, None);
    let server_endpoint = IndexedRelayEndpoint::new(
        b.clone(),
        None,
        Some(IndexedRelayTrust::Pinned(Arc::new(HashSet::from([
            client_identity.public().user_id(),
        ])))),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let channel = SecureChannel::accept(stream, &server_identity)
            .await
            .unwrap();
        server_endpoint.serve_connection(channel, None).await
    });
    let result = endpoint
        .sync_with_relay(addr, &client_identity, &expected)
        .await;
    if result.is_err() {
        server.abort();
    }
    assert_eq!(result.unwrap(), 1);
    server.await.unwrap().unwrap();
    assert!(a.durable_have(&second.conversation_id, &second.id).unwrap());
    assert!(b.durable_have(&first.conversation_id, &first.id).unwrap());
}
fn compatible_office() -> (tempfile::TempDir, IndexedPostOffice) {
    let (root, mut office) = office(DeviceIdentity::generate());
    office.state = Arc::new(
        crate::postoffice::storage::role::IndexedRelayState::from_profile(
            office.state.profile().clone(),
            crate::postoffice::storage::role::RelayAdmission::LegacyCompatible,
        )
        .unwrap(),
    );
    (root, office)
}
fn mailbox(n: u32) -> ConversationId {
    let mut key = [0; 32];
    key[..4].copy_from_slice(b"MTF3");
    key[28..].copy_from_slice(&n.to_be_bytes());
    ConversationId::new(key)
}
fn staged(n: u32) -> FederatedMailbox {
    FederatedMailbox {
        id: mailbox(n),
        digest: [1; 32],
        first_seen_secs: unix_now(),
    }
}
async fn channels() -> (SecureChannel<TcpStream>, SecureChannel<TcpStream>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (client, server) = tokio::join!(
        async {
            SecureChannel::connect(
                TcpStream::connect(addr).await.unwrap(),
                &DeviceIdentity::generate(),
                None,
            )
            .await
            .unwrap()
        },
        async {
            SecureChannel::accept(
                listener.accept().await.unwrap().0,
                &DeviceIdentity::generate(),
            )
            .await
            .unwrap()
        }
    );
    (client, server)
}
#[tokio::test]
async fn multipage_inventory_rotates_beyond_sixteen_actual_mailboxes() {
    let (_a, mut a) = compatible_office();
    let (_b, b) = compatible_office();
    let author = DeviceIdentity::generate();
    let mut ids = vec![];
    for n in 0..513 {
        let event = Event::new(
            &author,
            mailbox(n),
            1,
            vec![],
            1,
            1,
            crate::eventlog::EventKind::Message,
            vec![42],
        );
        ids.push((event.conversation_id, event.id));
        a.ingest(event).unwrap();
    }
    let client = DeviceIdentity::generate();
    let server = DeviceIdentity::generate();
    let expected = server.public();
    let endpoint = IndexedRelayEndpoint::new(a.clone(), None, None);
    let remote = IndexedRelayEndpoint::new(
        b.clone(),
        None,
        Some(IndexedRelayTrust::Pinned(Arc::new(HashSet::from([client
            .public()
            .user_id()])))),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        for _ in 0..2 {
            let channel = SecureChannel::accept(listener.accept().await.unwrap().0, &server)
                .await
                .unwrap();
            remote.serve_connection(channel, None).await.unwrap();
        }
    });
    for expected_count in [16, 32] {
        prepare(&a);
        prepare(&b);
        assert_eq!(
            endpoint
                .sync_with_relay(addr, &client, &expected)
                .await
                .unwrap(),
            16
        );
        let count = ids
            .iter()
            .filter(|(conv, id)| b.durable_have(conv, id).unwrap())
            .count();
        assert_eq!(count, expected_count);
    }
    task.await.unwrap();
    a.state.profile().database().read(|c| {
        let (pages,entries):(i64,i64)=c.query_row("SELECT local_pages,local_entries FROM relay_federation_sessions ORDER BY session_id LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert_eq!((pages,entries),(2,513)); Ok(())
    }).unwrap();
}
#[tokio::test]
async fn sql_and_legacy_federation_preserve_wire_compatibility_both_directions() {
    for sql_requests in [true, false] {
        let (_root, mut sql) = office(DeviceIdentity::generate());
        let old_root = tempfile::tempdir().unwrap();
        let old = Arc::new(Mutex::new(
            PostOffice::open(
                &old_root.path().join("relay.log"),
                "pw",
                DeviceIdentity::generate(),
            )
            .unwrap(),
        ));
        let author = DeviceIdentity::generate();
        let recipient = DeviceIdentity::generate();
        let first = opaque_event(1, &author, &recipient);
        let second = opaque_event(2, &author, &recipient);
        sql.ingest(first.clone()).unwrap();
        old.lock().unwrap().accept(second.clone()).unwrap();
        prepare(&sql);
        let client = DeviceIdentity::generate();
        let server = DeviceIdentity::generate();
        let expected = server.public();
        let endpoint = IndexedRelayEndpoint::new(
            sql.clone(),
            None,
            Some(IndexedRelayTrust::Pinned(Arc::new(HashSet::from([client
                .public()
                .user_id()])))),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let old_server = old.clone();
        let indexed_server = endpoint.clone();
        let task = tokio::spawn(async move {
            let mut channel = SecureChannel::accept(listener.accept().await.unwrap().0, &server)
                .await
                .unwrap();
            if sql_requests {
                let request = channel.recv().await.unwrap();
                let session = Mutex::new(
                    send_inventory(&mut channel, &old_server, None, &request)
                        .await
                        .unwrap(),
                );
                while matches!(
                    crate::node::session::serve_one(&mut channel, &session)
                        .await
                        .unwrap(),
                    Served::Handled(_)
                ) {}
            } else {
                indexed_server
                    .serve_connection(channel, None)
                    .await
                    .unwrap();
            }
        });
        let count = if sql_requests {
            endpoint
                .sync_with_relay(addr, &client, &expected)
                .await
                .unwrap()
        } else {
            super::super::sync_with_relay(addr, &client, &expected, &old)
                .await
                .unwrap()
        };
        assert_eq!(count, 1);
        task.await.unwrap();
        assert!(
            sql.durable_have(&second.conversation_id, &second.id)
                .unwrap()
        );
        assert!(
            old.lock()
                .unwrap()
                .durable_have(&first.conversation_id, &first.id)
                .unwrap()
        );
    }
}
#[tokio::test]
async fn actual_wire_rejects_trailing_bytes_missing_terminal_and_frame_ceiling() {
    for case in 0..3 {
        let (_root, office) = office(DeviceIdentity::generate());
        let owner = SessionOwner::begin(
            office.state.profile().clone(),
            Direction::Inbound,
            unix_now(),
            Instant::now(),
        )
        .unwrap();
        let lease = owner.lease();
        let (mut tx, mut rx) = channels().await;
        let sender = tokio::spawn(async move {
            match case {
                0 => {
                    let mut bytes = encode_page(&[staged(1)], true).unwrap();
                    bytes.push(0);
                    tx.send(&bytes).await.unwrap();
                }
                1 => {
                    tx.send(&encode_page(&[staged(1)], false).unwrap())
                        .await
                        .unwrap();
                }
                _ => {
                    for _ in 0..256 {
                        if tx.send(&encode_page(&[], false).unwrap()).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
        assert!(receive_inventory(&mut rx, &lease).await.is_err());
        sender.await.unwrap();
        assert!(
            office
                .state
                .profile()
                .database()
                .read(|c| lease.validate_on(c, true))
                .is_err()
        );
        drop(owner);
        while crate::postoffice::storage::federation::cleanup_step(
            office.state.profile(),
            unix_now(),
        )
        .unwrap()
        {}
        office
            .state
            .profile()
            .database()
            .read(|c| {
                let rows: i64 = c.query_row(
                    "SELECT session_rows FROM relay_federation_runtime",
                    [],
                    |r| r.get(0),
                )?;
                assert_eq!(rows, 0);
                Ok(())
            })
            .unwrap();
    }
}
#[tokio::test]
async fn untrusted_inventory_is_rejected_before_any_preparation_or_presence() {
    let (_root, office) = office(DeviceIdentity::generate());
    let endpoint = IndexedRelayEndpoint::new(
        office.clone(),
        Some(FederationDirectory::new()),
        Some(IndexedRelayTrust::Pinned(Arc::new(HashSet::new()))),
    );
    let (mut client, server) = channels().await;
    client.send(REQUEST_V4).await.unwrap();
    let error = endpoint.serve_connection(server, None).await.unwrap_err();
    assert!(error.contains("not authorized"));
    office
        .state
        .profile()
        .database()
        .read(|c| {
            let phase: i64 =
                c.query_row("SELECT phase FROM relay_digest_preparation", [], |r| {
                    r.get(0)
                })?;
            let sessions: i64 = c.query_row(
                "SELECT session_rows FROM relay_federation_runtime",
                [],
                |r| r.get(0),
            )?;
            assert_eq!((phase, sessions), (0, 0));
            Ok(())
        })
        .unwrap();
}
async fn sync_once(
    client_office: &IndexedPostOffice,
    server_office: &IndexedPostOffice,
) -> Result<usize, String> {
    prepare(client_office);
    prepare(server_office);
    let client = DeviceIdentity::generate();
    let server = DeviceIdentity::generate();
    let expected = server.public();
    let endpoint = IndexedRelayEndpoint::new(client_office.clone(), None, None);
    let remote = IndexedRelayEndpoint::new(
        server_office.clone(),
        None,
        Some(IndexedRelayTrust::Pinned(Arc::new(HashSet::from([client
            .public()
            .user_id()])))),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let channel = SecureChannel::accept(listener.accept().await.unwrap().0, &server)
            .await
            .unwrap();
        remote.serve_connection(channel, None).await
    });
    let result = endpoint.sync_with_relay(addr, &client, &expected).await;
    let _ = task.await.unwrap();
    result
}
#[tokio::test]
async fn earlier_known_origin_invalidates_sent_snapshot_then_bounded_retry_converges() {
    let (_a, mut a) = office(DeviceIdentity::generate());
    let (_b, b) = office(DeviceIdentity::generate());
    let event = opaque_event(1, &DeviceIdentity::generate(), &DeviceIdentity::generate());
    let old = unix_now() - 60;
    a.ingest(event.clone()).unwrap();
    b.state.record(&event, old, None).unwrap();
    assert!(sync_once(&a, &b).await.unwrap_err().contains("stale"));
    assert_eq!(
        a.state
            .mailbox_metadata_at(&event.conversation_id, unix_now())
            .unwrap()
            .unwrap()
            .first_seen,
        old
    );
    assert_eq!(sync_once(&a, &b).await.unwrap(), 0);
    assert!(a.durable_have(&event.conversation_id, &event.id).unwrap());
    a.state
        .profile()
        .database()
        .read(|c| {
            let rows: i64 = c.query_row(
                "SELECT session_rows FROM relay_federation_runtime",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(
                rows, 2,
                "failed attempt keeps charged staging until cleanup"
            );
            Ok(())
        })
        .unwrap();
}
#[tokio::test]
async fn rollback_cutoff_rejects_body_and_custody_after_known_origin_correction() {
    let (_a, a) = office(DeviceIdentity::generate());
    let (_b, b) = office(DeviceIdentity::generate());
    let event = opaque_event(1, &DeviceIdentity::generate(), &DeviceIdentity::generate());
    let expired = opaque_event(1, &DeviceIdentity::generate(), &DeviceIdentity::generate());
    let now = unix_now();
    a.state.record(&expired, now - 300, None).unwrap();
    a.state.record(&event, now, None).unwrap();
    b.state.record(&event, now - 200, None).unwrap();
    assert!(
        a.state
            .mailbox_metadata_at(
                &expired.conversation_id,
                now + MAX_RELAY_AGE.as_secs() - 100
            )
            .is_err()
    );
    assert!(sync_once(&a, &b).await.is_err());
    assert!(matches!(
        a.durable_have(&event.conversation_id, &event.id),
        Err(LogError::RelayMailboxUnavailable)
    ));
    assert!(a.open_view(&event.conversation_id).is_err());
    assert_eq!(
        a.state
            .mailbox_metadata(&event.conversation_id)
            .unwrap()
            .unwrap()
            .first_seen,
        now - 200
    );
}
#[tokio::test]
async fn actual_multihop_custody_preserves_oldest_origin() {
    let (_a, a) = office(DeviceIdentity::generate());
    let (_b, b) = office(DeviceIdentity::generate());
    let (_c, c) = office(DeviceIdentity::generate());
    let event = opaque_event(1, &DeviceIdentity::generate(), &DeviceIdentity::generate());
    let first = unix_now() - 200;
    a.state.record(&event, first, None).unwrap();
    assert_eq!(sync_once(&b, &a).await.unwrap(), 1);
    assert_eq!(sync_once(&c, &b).await.unwrap(), 1);
    for office in [&b, &c] {
        assert_eq!(
            office
                .state
                .mailbox_metadata_at(&event.conversation_id, unix_now())
                .unwrap()
                .unwrap()
                .first_seen,
            first
        );
        assert!(
            office
                .durable_have(&event.conversation_id, &event.id)
                .unwrap()
        );
    }
}
#[tokio::test]
async fn cache_overwrite_after_send_keeps_actual_selected_inventory_equality() {
    let (_root, mut office) = office(DeviceIdentity::generate());
    let author = DeviceIdentity::generate();
    let recipient = DeviceIdentity::generate();
    let first = opaque_event(1, &author, &recipient);
    let second = opaque_event(2, &author, &recipient);
    office.ingest(first.clone()).unwrap();
    prepare(&office);
    let client = DeviceIdentity::generate();
    let server = DeviceIdentity::generate();
    let expected = server.public();
    let endpoint = IndexedRelayEndpoint::new(office.clone(), None, None);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let changed = office.clone();
    let task = tokio::spawn(async move {
        let mut channel = SecureChannel::accept(listener.accept().await.unwrap().0, &server)
            .await
            .unwrap();
        assert_eq!(channel.recv().await.unwrap(), REQUEST_V4);
        receive_presence_pages(&mut channel, None, presence_limit(REQUEST_V4), true, true)
            .await
            .unwrap();
        let selected = receive_inventory_pages(&mut channel).await.unwrap();
        assert_eq!(selected.len(), 1);
        changed.state.record(&second, unix_now(), None).unwrap();
        prepare(&changed);
        send_inventory_pages(&mut channel, &selected).await.unwrap();
        send_presence_pages(&mut channel, None, presence_limit(REQUEST_V4), true, true)
            .await
            .unwrap();
    });
    assert_eq!(
        endpoint
            .sync_with_relay(addr, &client, &expected)
            .await
            .unwrap(),
        0
    );
    task.await.unwrap();
    assert_eq!(
        office
            .metadata(&office.open_view(&first.conversation_id).unwrap())
            .unwrap()
            .count,
        2
    );
}
#[tokio::test]
async fn three_hundred_raw_empty_slices_emit_one_actual_terminal_frame() {
    let (_root, mut office) = compatible_office();
    let signer = DeviceIdentity::generate();
    let event = Event::new(
        &signer,
        mailbox(1),
        1,
        vec![],
        1,
        1,
        crate::eventlog::EventKind::Message,
        vec![42],
    );
    office.ingest(event).unwrap();
    prepare(&office);
    // Selector-only metadata fixture: these nonfederated rows are skipped before
    // repository eligibility/body lookup. This isolates 300 raw 512-key slices.
    office.state.profile().database().transaction(|c| {
        let mut q=c.prepare("INSERT INTO relay_mailboxes(domain,conversation,first_seen,touched) VALUES(?1,?2,?3,?3)")?;
        for n in 0..(300*512u32) {
            let mut key=[0u8;32]; key[28..].copy_from_slice(&n.to_be_bytes());
            q.execute(rusqlite::params![crate::storage::profile::context::RELAY_ARRIVALS.as_slice(),key.as_slice(),unix_now().to_be_bytes().as_slice()])?;
        } Ok(())
    }).unwrap();
    let endpoint = IndexedRelayEndpoint::new(office.clone(), None, None);
    let owner = SessionOwner::begin(
        office.state.profile().clone(),
        Direction::Outbound,
        unix_now(),
        Instant::now(),
    )
    .unwrap();
    let lease = owner.lease();
    let view = office.state.inventory_view_at(unix_now()).unwrap();
    let (mut tx, mut rx) = channels().await;
    let receive = tokio::spawn(async move { decode_page(&rx.recv().await.unwrap()).unwrap() });
    endpoint
        .send_inventory(&mut tx, &lease, &view)
        .await
        .unwrap();
    let (entries, last) = receive.await.unwrap();
    assert_eq!(entries.len(), 1);
    assert!(last);
    office
        .state
        .profile()
        .database()
        .read(|c| {
            let row: (i64, i64) = c.query_row(
                "SELECT local_pages,local_framed_entries FROM relay_federation_sessions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(row, (1, 1));
            Ok(())
        })
        .unwrap();
}
#[tokio::test]
async fn global_staging_pressure_fails_actual_receive_without_partial_reservation() {
    let (_root, office) = office(DeviceIdentity::generate());
    let owner = SessionOwner::begin(
        office.state.profile().clone(),
        Direction::Inbound,
        unix_now(),
        Instant::now(),
    )
    .unwrap();
    let lease = owner.lease();
    office
        .state
        .profile()
        .database()
        .transaction(|c| {
            c.execute(
                "UPDATE relay_federation_runtime SET staged_rows=262144,staged_bytes=50332672",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let (mut tx, mut rx) = channels().await;
    tx.send(&encode_page(&[staged(1)], true).unwrap())
        .await
        .unwrap();
    assert!(matches!(
        receive_inventory(&mut rx, &lease).await.unwrap_err(),
        IndexedRelayError::Store(LogError::RelayCapacityUnavailable)
    ));
    office
        .state
        .profile()
        .database()
        .read(|c| {
            let row: (i64, i64) = c.query_row(
                "SELECT receive_pages,row_charge FROM relay_federation_sessions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(row, (0, 0));
            Ok(())
        })
        .unwrap();
}
#[tokio::test]
async fn one_endpoint_have_gate_is_shared_by_clones_and_actual_rounds() {
    let (_a, mut a) = office(DeviceIdentity::generate());
    let (_b, mut b) = office(DeviceIdentity::generate());
    let author = DeviceIdentity::generate();
    let recipient = DeviceIdentity::generate();
    let first = opaque_event(1, &author, &recipient);
    let second = opaque_event(2, &author, &recipient);
    a.ingest(first.clone()).unwrap();
    b.ingest(second.clone()).unwrap();
    prepare(&a);
    prepare(&b);
    let client = DeviceIdentity::generate();
    let server = DeviceIdentity::generate();
    let expected = server.public();
    let endpoint = IndexedRelayEndpoint::new(a.clone(), None, None);
    let clone = endpoint.clone();
    let mut permits: Vec<_> = (0..4).map(|_| clone.budget.try_admit().unwrap()).collect();
    assert!(endpoint.budget.try_admit().is_err());
    let remote = IndexedRelayEndpoint::new(
        b.clone(),
        None,
        Some(IndexedRelayTrust::Pinned(Arc::new(HashSet::from([client
            .public()
            .user_id()])))),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        for _ in 0..2 {
            let channel = SecureChannel::accept(listener.accept().await.unwrap().0, &server)
                .await
                .unwrap();
            let _ = remote.serve_connection(channel, None).await;
        }
    });
    assert!(
        endpoint
            .sync_with_relay(addr, &client, &expected)
            .await
            .unwrap_err()
            .contains("resources unavailable")
    );
    assert_eq!(
        a.state.federation_cursor(&expected),
        Some(first.conversation_id)
    );
    drop(permits.pop());
    prepare(&a);
    prepare(&b);
    assert_eq!(
        clone
            .sync_with_relay(addr, &client, &expected)
            .await
            .unwrap(),
        1
    );
    task.await.unwrap();
    assert!(a.durable_have(&second.conversation_id, &second.id).unwrap());
}
#[tokio::test]
async fn slow_trusted_inventory_cannot_renew_deadline_or_block_healthy_edge() {
    let (_root, office) = office(DeviceIdentity::generate());
    prepare(&office);
    let (mut slow, server) = channels().await;
    let slow_id = server.peer_identity().user_id();
    let healthy = DeviceIdentity::generate();
    let relay = DeviceIdentity::generate();
    let expected = relay.public();
    let endpoint = IndexedRelayEndpoint::new(
        office.clone(),
        None,
        Some(IndexedRelayTrust::Pinned(Arc::new(HashSet::from([
            slow_id,
            healthy.public().user_id(),
        ])))),
    );
    let slow_endpoint = endpoint.clone();
    let slow_task = tokio::spawn(async move { slow_endpoint.serve_connection(server, None).await });
    slow.send(REQUEST_V4).await.unwrap();
    send_presence_pages(&mut slow, None, presence_limit(REQUEST_V4), true, true)
        .await
        .unwrap();
    slow.send(&encode_page(&[staged(9)], false).unwrap())
        .await
        .unwrap();
    loop {
        let received: i64 = office
            .state
            .profile()
            .database()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT COALESCE(sum(receive_pages),0) FROM relay_federation_sessions",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        if received == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    let (_healthy_root, healthy_office) = office_fixture_for_empty();
    prepare(&healthy_office);
    let outgoing = IndexedRelayEndpoint::new(healthy_office, None, None);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let healthy_endpoint = endpoint.clone();
    let healthy_task = tokio::spawn(async move {
        let channel = SecureChannel::accept(listener.accept().await.unwrap().0, &relay)
            .await
            .unwrap();
        healthy_endpoint.serve_connection(channel, None).await
    });
    assert_eq!(
        outgoing
            .sync_with_relay(addr, &healthy, &expected)
            .await
            .unwrap(),
        0
    );
    healthy_task.await.unwrap().unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(29)).await;
    slow.send(&encode_page(&[], false).unwrap()).await.unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(
        slow_task
            .await
            .unwrap()
            .unwrap_err()
            .contains("deadline exceeded")
    );
    while crate::postoffice::storage::federation::cleanup_step(office.state.profile(), unix_now())
        .unwrap()
    {}
    office
        .state
        .profile()
        .database()
        .read(|c| {
            let rows: i64 = c.query_row(
                "SELECT session_rows FROM relay_federation_runtime",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(rows, 0);
            Ok(())
        })
        .unwrap();
}
fn office_fixture_for_empty() -> (tempfile::TempDir, IndexedPostOffice) {
    office(DeviceIdentity::generate())
}
#[tokio::test]
async fn cancelling_actual_exchange_revokes_authority_and_cleanup_releases_charge() {
    let (_root, office) = office(DeviceIdentity::generate());
    prepare(&office);
    let (mut client, server) = channels().await;
    let endpoint = IndexedRelayEndpoint::new(
        office.clone(),
        None,
        Some(IndexedRelayTrust::Pinned(Arc::new(HashSet::from([server
            .peer_identity()
            .user_id()])))),
    );
    let task = tokio::spawn(async move { endpoint.serve_connection(server, None).await });
    client.send(REQUEST_V4).await.unwrap();
    send_presence_pages(&mut client, None, presence_limit(REQUEST_V4), true, true)
        .await
        .unwrap();
    client
        .send(&encode_page(&[staged(7)], false).unwrap())
        .await
        .unwrap();
    loop {
        let rows: i64 = office
            .state
            .profile()
            .database()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT staged_rows FROM relay_federation_runtime",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        if rows == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    while crate::postoffice::storage::federation::cleanup_step(office.state.profile(), unix_now())
        .unwrap()
    {}
    office
        .state
        .profile()
        .database()
        .read(|c| {
            let (rows, bytes): (i64, i64) = c.query_row(
                "SELECT staged_rows,staged_bytes FROM relay_federation_runtime",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!((rows, bytes), (0, 0));
            Ok(())
        })
        .unwrap();
}
#[tokio::test]
async fn invalid_approved_network_cannot_disclose_metadata_or_fall_back_to_pins() {
    use crate::postoffice::network::{NetworkMembership, NetworkPolicy, RelayNetwork};
    let (_a, a) = office(DeviceIdentity::generate());
    let (_b, b) = office(DeviceIdentity::generate());
    prepare(&a);
    let client = DeviceIdentity::generate();
    let server = DeviceIdentity::generate();
    let expected = server.public();
    let authority = DeviceIdentity::generate();
    let other_authority = DeviceIdentity::generate();
    let now = unix_now();
    let client_network = RelayNetwork::new(
        NetworkPolicy::new(authority.public().ed25519_pub),
        NetworkMembership::issue(
            &authority,
            &client.public().user_id(),
            now,
            Duration::from_secs(3600),
        )
        .unwrap(),
        &client.public(),
        now,
    )
    .unwrap();
    let server_network = RelayNetwork::new(
        NetworkPolicy::new(other_authority.public().ed25519_pub),
        NetworkMembership::issue(
            &other_authority,
            &server.public().user_id(),
            now,
            Duration::from_secs(3600),
        )
        .unwrap(),
        &server.public(),
        now,
    )
    .unwrap();
    let endpoint =
        IndexedRelayEndpoint::new(a, None, Some(IndexedRelayTrust::Network(client_network)));
    let remote = IndexedRelayEndpoint::new(
        b.clone(),
        Some(FederationDirectory::new()),
        Some(IndexedRelayTrust::Network(server_network)),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let channel = SecureChannel::accept(listener.accept().await.unwrap().0, &server)
            .await
            .unwrap();
        assert!(remote.serve_connection(channel, None).await.is_err());
        // A retry would reconnect to this still-bound socket. No failed network
        // authorization may restart the legacy pin-only federation protocol.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err()
        );
    });
    assert!(
        endpoint
            .sync_with_relay(addr, &client, &expected)
            .await
            .is_err()
    );
    task.await.unwrap();
    b.state
        .profile()
        .database()
        .read(|c| {
            let phase: i64 =
                c.query_row("SELECT phase FROM relay_digest_preparation", [], |r| {
                    r.get(0)
                })?;
            let rows: i64 = c.query_row(
                "SELECT session_rows FROM relay_federation_runtime",
                [],
                |r| r.get(0),
            )?;
            assert_eq!((phase, rows), (0, 0));
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn ordinary_opaque_client_uses_sql_endpoint_without_full_inventory_preparation() {
    let (_root, mut office) = office(DeviceIdentity::generate());
    let author = DeviceIdentity::generate();
    let recipient = DeviceIdentity::generate();
    let first = opaque_event(1, &author, &recipient);
    let second = opaque_event(2, &author, &recipient);
    office.ingest(first.clone()).unwrap();
    let endpoint = IndexedRelayEndpoint::new(office.clone(), None, None);
    let (mut client, server) = channels().await;
    let task = tokio::spawn(async move { endpoint.serve_connection(server, None).await });
    let mut log = crate::eventlog::EventLog::default();
    log.append(second.clone()).unwrap();
    let store = Mutex::new(log);
    request_round(&mut client, &store, first.conversation_id)
        .await
        .unwrap();
    drop(client);
    task.await.unwrap().unwrap();
    assert!(
        office
            .durable_have(&second.conversation_id, &second.id)
            .unwrap()
    );
    assert_eq!(store.lock().unwrap().get(&first.id), Some(&first));
    office
        .state
        .profile()
        .database()
        .read(|c| {
            let phase: i64 =
                c.query_row("SELECT phase FROM relay_digest_preparation", [], |r| {
                    r.get(0)
                })?;
            assert_eq!(
                phase, 0,
                "ordinary scoped sync never starts full inventory discovery"
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn preparation_miss_is_a_typed_local_outcome_before_version_negotiation() {
    let (_root, office) = office(DeviceIdentity::generate());
    let endpoint = IndexedRelayEndpoint::new(office, None, None);
    let error = endpoint.prepared().unwrap_err();
    assert_eq!(format!("{error:?}"), "PreparationPending");
}

#[tokio::test]
async fn local_commit_fault_before_connect_keeps_typed_primary_and_cannot_downgrade() {
    let (_root, office) = office(DeviceIdentity::generate());
    prepare(&office);
    office.state.profile().database().read(|c| {
        c.execute_batch("CREATE TEMP TABLE endpoint_fault_parent(id INTEGER PRIMARY KEY); CREATE TEMP TABLE endpoint_fault_child(id INTEGER REFERENCES endpoint_fault_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TEMP TRIGGER endpoint_commit_fault AFTER INSERT ON relay_federation_sessions BEGIN INSERT INTO endpoint_fault_child VALUES(1); END;")?;Ok(())
    }).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = IndexedRelayEndpoint::new(office.clone(), None, None);
    let error = endpoint
        .sync_with_relay_checked(
            listener.local_addr().unwrap(),
            &DeviceIdentity::generate(),
            &DeviceIdentity::generate().public(),
        )
        .await
        .unwrap_err();
    assert!(!error.is_local_retry());
    assert!(matches!(
        error,
        IndexedRelayError::Store(LogError::Database(_))
    ));
    assert!(office.requires_reopen());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err(),
        "local COMMIT failure must not open or retry any wire version"
    );
}
