//! An enrollment certificate grants relay membership, never software integrity.
use mesh_talk_core::identity::device::DeviceIdentity;
use mesh_talk_core::postoffice::network::{NetworkMembership, NetworkPolicy, RelayNetwork};
use std::time::Duration;

fn enrolled(authority: &DeviceIdentity, relay: &DeviceIdentity) -> RelayNetwork {
    let now = mesh_talk_core::postoffice::network::unix_now();
    RelayNetwork::new(
        NetworkPolicy::new(authority.public().ed25519_pub),
        NetworkMembership::issue(authority, &relay.user_id(), now, Duration::from_secs(3600))
            .unwrap(),
        &relay.public(),
        now,
    )
    .unwrap()
}

#[tokio::test]
async fn approved_relays_exchange_opaque_mail_without_individual_pins() {
    use mesh_talk_core::eventlog::event::{ConversationId, Event, EventKind};
    use mesh_talk_core::node::run_network_relay_accept_loop_with_directory;
    use mesh_talk_core::postoffice::{
        PostOffice, federation::FederationDirectory, opaque::RelayMailbox,
    };
    use std::sync::{Arc, Mutex};
    let temp = tempfile::tempdir().unwrap();
    let authority = DeviceIdentity::generate();
    let first = DeviceIdentity::generate();
    let second = DeviceIdentity::generate();
    let first_network = enrolled(&authority, &first);
    let second_network = enrolled(&authority, &second);
    let second_public = second.public();
    let first_store = Arc::new(Mutex::new(
        PostOffice::open(&temp.path().join("first"), "pw", DeviceIdentity::generate()).unwrap(),
    ));
    let second_store = Arc::new(Mutex::new(
        PostOffice::open(
            &temp.path().join("second"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap(),
    ));
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let event = Event::new(
        &alice,
        ConversationId::new([61; 32]),
        1,
        vec![],
        1,
        10,
        EventKind::Message,
        b"secret".to_vec(),
    );
    let outer = RelayMailbox::federated(&alice, &bob.public())
        .unwrap()
        .wrap(&event)
        .unwrap();
    first_store.lock().unwrap().accept(outer.clone()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(run_network_relay_accept_loop_with_directory(
        second,
        listener,
        Arc::clone(&second_store),
        second_network,
        FederationDirectory::new(),
    ));
    let directory = FederationDirectory::new();
    let result = directory
        .sync_with_network_relay(addr, &first, &second_public, &first_store, &first_network)
        .await;
    server.abort();
    let _ = server.await;
    result.unwrap();
    assert!(second_store.lock().unwrap().has(&outer.id));
}

#[tokio::test]
async fn wrong_network_and_bare_legacy_inventory_are_denied() {
    use mesh_talk_core::node::run_network_relay_accept_loop_with_directory;
    use mesh_talk_core::postoffice::{PostOffice, federation::FederationDirectory};
    use mesh_talk_core::transport::SecureChannel;
    use std::sync::{Arc, Mutex};
    let temp = tempfile::tempdir().unwrap();
    let first = DeviceIdentity::generate();
    let second = DeviceIdentity::generate();
    let first_network = enrolled(&DeviceIdentity::generate(), &first);
    let second_network = enrolled(&DeviceIdentity::generate(), &second);
    let expected = second.public();
    let store = Arc::new(Mutex::new(
        PostOffice::open(
            &temp.path().join("second"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap(),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(run_network_relay_accept_loop_with_directory(
        second,
        listener,
        store,
        second_network,
        FederationDirectory::new(),
    ));
    for request in [Some(&first_network), None] {
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut channel = SecureChannel::connect(stream, &first, Some(&expected))
            .await
            .unwrap();
        if let Some(network) = request {
            assert!(network.connect(&mut channel).await.is_err());
        } else {
            channel.send(b"MTFI2Q").await.unwrap();
            assert!(
                channel.recv().await.is_err(),
                "no inventory may precede membership approval"
            );
        }
    }
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn invalid_memberships_disclose_no_inventory_on_authenticated_connections() {
    use mesh_talk_core::node::run_network_relay_accept_loop_with_directory;
    use mesh_talk_core::postoffice::{
        PostOffice, federation::FederationDirectory, network::unix_now,
    };
    use mesh_talk_core::transport::SecureChannel;
    use std::sync::{Arc, Mutex};
    let temp = tempfile::tempdir().unwrap();
    let authority = DeviceIdentity::generate();
    let caller = DeviceIdentity::generate();
    let impostor = DeviceIdentity::generate();
    let now = unix_now();
    let valid = NetworkMembership::issue(
        &authority,
        &caller.user_id(),
        now,
        Duration::from_secs(3600),
    )
    .unwrap()
    .encode()
    .unwrap();
    for kind in [
        "expired",
        "forged",
        "copied",
        "revoked",
        "oversized",
        "trailing",
    ] {
        let server_identity = DeviceIdentity::generate();
        let expected = server_identity.public();
        let mut policy = NetworkPolicy::new(authority.public().ed25519_pub);
        if kind == "revoked" {
            policy.revoke(&caller.user_id()).unwrap();
        }
        let own = NetworkMembership::issue(
            &authority,
            &server_identity.user_id(),
            now,
            Duration::from_secs(3600),
        )
        .unwrap();
        let server_network = RelayNetwork::new(policy, own, &expected, now).unwrap();
        let store = Arc::new(Mutex::new(
            PostOffice::open(&temp.path().join(kind), "pw", DeviceIdentity::generate()).unwrap(),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(run_network_relay_accept_loop_with_directory(
            server_identity,
            listener,
            store,
            server_network,
            FederationDirectory::new(),
        ));
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let identity = if kind == "copied" { &impostor } else { &caller };
        let mut channel = SecureChannel::connect(stream, identity, Some(&expected))
            .await
            .unwrap();
        let mut certificate = match kind {
            "expired" => NetworkMembership::issue(
                &authority,
                &caller.user_id(),
                now - 120,
                Duration::from_secs(60),
            )
            .unwrap()
            .encode()
            .unwrap(),
            "oversized" => vec![0; 4097],
            _ => valid.clone(),
        };
        if kind == "forged" {
            *certificate.last_mut().unwrap() ^= 1;
        }
        if kind == "trailing" {
            certificate.push(0);
        }
        let mut request = b"MTNM1Q".to_vec();
        request.extend(certificate);
        channel.send(&request).await.unwrap();
        let response = tokio::time::timeout(Duration::from_secs(2), channel.recv())
            .await
            .unwrap();
        server.abort();
        let _ = server.await;
        assert!(response.is_err(), "{kind} membership disclosed a response");
    }
}

#[tokio::test]
async fn requester_rejects_a_server_with_a_copied_membership_before_sending_inventory() {
    use mesh_talk_core::postoffice::network::unix_now;
    use mesh_talk_core::transport::SecureChannel;
    let authority = DeviceIdentity::generate();
    let caller = DeviceIdentity::generate();
    let server_identity = DeviceIdentity::generate();
    let expected = server_identity.public();
    let network = enrolled(&authority, &caller);
    let copied = NetworkMembership::issue(
        &authority,
        &caller.user_id(),
        unix_now(),
        Duration::from_secs(3600),
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut channel = SecureChannel::accept(stream, &server_identity)
            .await
            .unwrap();
        assert!(channel.recv().await.unwrap().starts_with(b"MTNM1Q"));
        let mut response = b"MTNM1R".to_vec();
        response.extend(copied.encode().unwrap());
        channel.send(&response).await.unwrap();
        assert!(
            channel.recv().await.is_err(),
            "requester disclosed inventory to an impersonating server"
        );
    });
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut channel = SecureChannel::connect(stream, &caller, Some(&expected))
        .await
        .unwrap();
    assert!(network.connect(&mut channel).await.is_err());
    drop(channel);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}
