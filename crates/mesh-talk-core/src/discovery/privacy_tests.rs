use super::*;
use crate::identity::device::DeviceIdentity;

#[tokio::test]
async fn private_listener_records_a_peer_without_disclosing_identity_reply() {
    let peer_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let peer_port = peer_socket.local_addr().unwrap().port();
    let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let address = socket.local_addr().unwrap();
    let roster = Arc::new(Mutex::new(Roster::default()));
    let me = DeviceIdentity::generate();
    let peer = DeviceIdentity::generate();
    let handle = tokio::spawn(run_listen_with_visibility(
        socket,
        roster.clone(),
        me.user_id(),
        shared_announce(&Announce::new(&me, "Private", 1)),
        peer_port,
        Arc::new(DiscoveryVisibility::new(false)),
    ));
    peer_socket
        .send_to(&encode(&Announce::new(&peer, "Peer", 4000)), address)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while roster.lock().unwrap().get(&peer.user_id()).is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("private listener still discovers peers");
    let mut buf = [0; 2048];
    let reply =
        tokio::time::timeout(Duration::from_millis(150), peer_socket.recv_from(&mut buf)).await;
    handle.abort();
    let _ = handle.await;
    assert!(
        reply.is_err(),
        "private listener disclosed its identity in a reply"
    );
}

#[tokio::test]
async fn visibility_gates_startup_periodic_trigger_and_live_rename() {
    let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let sender = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let identity = DeviceIdentity::generate();
    let announce = shared_announce(&Announce::new(&identity, "Private", 4000));
    let trigger = Arc::new(Notify::new());
    let visibility = Arc::new(DiscoveryVisibility::new(false));
    let handle = tokio::spawn(run_broadcast_with_visibility(
        sender,
        announce.clone(),
        receiver.local_addr().unwrap(),
        Duration::from_millis(30),
        Some(trigger.clone()),
        visibility.clone(),
    ));
    let mut buf = [0; 2048];
    // All three startup delays total 2.6 seconds. Observe past the burst and
    // several periodic ticks, rather than only checking the first 100 ms send.
    assert!(
        tokio::time::timeout(Duration::from_millis(2800), receiver.recv_from(&mut buf))
            .await
            .is_err(),
        "private startup/periodic broadcast leaked"
    );
    trigger.notify_waiters();
    assert!(
        tokio::time::timeout(Duration::from_millis(120), receiver.recv_from(&mut buf))
            .await
            .is_err(),
        "a manual trigger bypassed private visibility"
    );
    swap_announce(&announce, &Announce::new(&identity, "Public renamed", 4000));
    visibility.set_public(true).await;
    trigger.notify_waiters();
    let (n, _) = tokio::time::timeout(Duration::from_secs(2), receiver.recv_from(&mut buf))
        .await
        .expect("public mode resumes without task restart")
        .unwrap();
    let decoded = decode(&buf[..n]).unwrap();
    assert!(decoded.verify());
    assert_eq!(decoded.name, "Public renamed");
    visibility.set_public(false).await;
    // Previously queued packets cannot be retracted; drain them before checking
    // that subsequent periodic sends and another manual trigger stay suppressed.
    while receiver.try_recv_from(&mut buf).is_ok() {}
    trigger.notify_waiters();
    let got = tokio::time::timeout(Duration::from_millis(200), receiver.recv_from(&mut buf)).await;
    handle.abort();
    let _ = handle.await;
    assert!(
        got.is_err(),
        "public-to-private transition allowed another announce"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scan_rechecks_visibility_between_targets_and_stops_the_sweep() {
    let first = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let second = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let third = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let visibility = DiscoveryVisibility::default();
    let mut visited = 0;
    let targets = [
        first.local_addr().unwrap(),
        second.local_addr().unwrap(),
        third.local_addr().unwrap(),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, address)| {
        visited += 1;
        if index == 1 {
            // Deterministically change the real control between target one
            // and target two; no UDP mock or real LAN sweep is involved.
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(visibility.set_public(false));
            });
        }
        address
    });
    assert!(!scan_addresses(&sender, b"announce", targets, &visibility).await);
    assert_eq!(
        visited, 2,
        "sweep must stop instead of visiting every private target"
    );
    let mut buf = [0; 128];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), first.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap()
            .0,
        b"announce".len()
    );
    for receiver in [second, third] {
        assert!(
            tokio::time::timeout(Duration::from_millis(100), receiver.recv_from(&mut buf))
                .await
                .is_err(),
            "a later scan target received our identity"
        );
    }
}

#[tokio::test]
async fn private_scan_sends_nothing_even_when_targets_are_known() {
    let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    assert!(
        !scan_addresses(
            &sender,
            b"announce",
            [receiver.local_addr().unwrap()],
            &DiscoveryVisibility::new(false)
        )
        .await
    );
    let mut buf = [0; 128];
    assert!(
        tokio::time::timeout(Duration::from_millis(100), receiver.recv_from(&mut buf))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn public_listener_can_be_disabled_live_while_passive_discovery_continues() {
    let peer_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let address = socket.local_addr().unwrap();
    let me = DeviceIdentity::generate();
    let first_peer = DeviceIdentity::generate();
    let later_peer = DeviceIdentity::generate();
    let roster = Arc::new(Mutex::new(Roster::default()));
    let visibility = Arc::new(DiscoveryVisibility::default());
    let handle = tokio::spawn(run_listen_with_visibility(
        socket,
        roster.clone(),
        me.user_id(),
        shared_announce(&Announce::new(&me, "Self", 4000)),
        peer_socket.local_addr().unwrap().port(),
        visibility.clone(),
    ));
    peer_socket
        .send_to(&encode(&Announce::new(&first_peer, "First", 4001)), address)
        .await
        .unwrap();
    let mut buf = [0; 2048];
    let (n, _) = tokio::time::timeout(Duration::from_secs(2), peer_socket.recv_from(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decode(&buf[..n]).unwrap().user_id, me.user_id());
    visibility.set_public(false).await;
    peer_socket
        .send_to(&encode(&Announce::new(&later_peer, "Later", 4002)), address)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while roster.lock().unwrap().get(&later_peer.user_id()).is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let got =
        tokio::time::timeout(Duration::from_millis(150), peer_socket.recv_from(&mut buf)).await;
    handle.abort();
    let _ = handle.await;
    assert!(got.is_err());
}

#[tokio::test]
async fn physical_scope_blocks_off_lan_queries_and_raw_announces_then_allows_live_opt_out() {
    let me = Arc::new(DeviceIdentity::generate());
    let peer = DeviceIdentity::generate();
    let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let listener = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let target = listener.local_addr().unwrap();
    let roster = Arc::new(Mutex::new(Roster::default()));
    let visibility = Arc::new(DiscoveryVisibility::with_inventory(true, || {
        vec![crate::discovery::scope::LanInterface {
            address: "192.168.3.1".parse().unwrap(),
            prefix: 24,
            index: Some(1),
        }]
    }));
    visibility.initialize_physical_lan_only(true).unwrap();
    let routed = Arc::new(RoutedDiscovery::new({
        let me = me.clone();
        Arc::new(move |nonce, announce| query::encode_response(nonce, announce, &me))
    }));
    let task = tokio::spawn(run_listen_with_routed(
        listener,
        roster.clone(),
        me.user_id(),
        shared_announce(&Announce::new(&me, "Scoped", 1)),
        sender.local_addr().unwrap().port(),
        visibility.clone(),
        Some(routed),
    ));
    let mut buf = [0; 2048];
    sender
        .send_to(&query::encode_query([8; 16]), target)
        .await
        .unwrap();
    sender
        .send_to(&encode(&Announce::new(&peer, "Off LAN", 2)), target)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(150), sender.recv_from(&mut buf))
            .await
            .is_err()
    );
    assert!(roster.lock().unwrap().get(&peer.user_id()).is_none());
    visibility.set_physical_lan_only(false).await;
    sender
        .send_to(&query::encode_query([9; 16]), target)
        .await
        .unwrap();
    let (n, _) = tokio::time::timeout(Duration::from_secs(2), sender.recv_from(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert!(buf[..n].starts_with(b"MTDR"));
    visibility.set_physical_lan_only(true).await;
    sender
        .send_to(&encode(&Announce::new(&peer, "Off LAN", 2)), target)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(150), sender.recv_from(&mut buf))
            .await
            .is_err()
    );
    assert!(roster.lock().unwrap().get(&peer.user_id()).is_none());
    task.abort();
}

#[tokio::test]
async fn physical_scope_on_lan_listener_still_replies() {
    // The injected inventory treats loopback as the actual LAN and proves LAN
    // query/reply continues even though generic routed probing is disabled.
    let me = Arc::new(DeviceIdentity::generate());
    let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let listener = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let visibility = Arc::new(DiscoveryVisibility::with_inventory(true, || {
        vec![crate::discovery::scope::LanInterface {
            address: "127.0.0.1".parse().unwrap(),
            prefix: 8,
            index: Some(1),
        }]
    }));
    visibility.initialize_physical_lan_only(true).unwrap();
    let target = listener.local_addr().unwrap();
    let routed = Arc::new(RoutedDiscovery::new({
        let me = me.clone();
        Arc::new(move |nonce, announce| query::encode_response(nonce, announce, &me))
    }));
    let task = tokio::spawn(run_listen_with_routed(
        listener,
        Arc::new(Mutex::new(Roster::default())),
        me.user_id(),
        shared_announce(&Announce::new(&me, "LAN", 1)),
        sender.local_addr().unwrap().port(),
        visibility.clone(),
        Some(routed),
    ));
    sender
        .send_to(&query::encode_query([4; 16]), target)
        .await
        .unwrap();
    let mut buf = [0; 2048];
    let (n, _) = tokio::time::timeout(Duration::from_secs(2), sender.recv_from(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert!(buf[..n].starts_with(b"MTDR"));
    assert!(!visibility.routed_allowed().await);
    task.abort();
}

#[tokio::test]
async fn physical_scope_suppresses_startup_and_manual_announces_to_off_lan_target() {
    let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let sender = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let visibility = Arc::new(DiscoveryVisibility::with_inventory(true, Vec::new));
    visibility.initialize_physical_lan_only(true).unwrap();
    let trigger = Arc::new(Notify::new());
    let me = DeviceIdentity::generate();
    let task = tokio::spawn(run_broadcast_with_visibility(
        sender,
        shared_announce(&Announce::new(&me, "Scoped", 1)),
        receiver.local_addr().unwrap(),
        Duration::from_secs(3600),
        Some(trigger.clone()),
        visibility,
    ));
    tokio::task::yield_now().await;
    trigger.notify_one();
    let mut buf = [0; 2048];
    assert!(
        tokio::time::timeout(Duration::from_millis(650), receiver.recv_from(&mut buf))
            .await
            .is_err(),
        "startup offsets and manual trigger must honor persisted scope"
    );
    task.abort();
}
