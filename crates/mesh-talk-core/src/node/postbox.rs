//! Post-office integration: find the elected post office in the roster, and run
//! the relay's server side (sync rounds over a durable [`PostOffice`]). The
//! Client upload and drain live in `opaque_relay` and `serving`;
//! this module provides election lookup and the serving loop.

use crate::discovery::Announce;
use crate::discovery::roster::{PeerRecord, Roster};
use crate::identity::device::DeviceIdentity;
use crate::node::bridge_directory::{self, BridgeDirectory};
use crate::node::session::{Served, serve_one, serve_wire_bytes};
use crate::node::transport::secure_accept;
use crate::postoffice::{PostOffice, elect};
use crate::transport::SecureChannel;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

/// The post office this node should use: the lowest-fingerprint peer among those
/// advertising the post-office role, or `None` if no post office is known.
/// Deterministic — every node computes the same winner from the same roster.
pub fn elected_post_office(roster: &Roster) -> Option<PeerRecord> {
    let candidates = roster.post_offices();
    let winner = elect(
        &candidates
            .iter()
            .map(|r| r.public.clone())
            .collect::<Vec<_>>(),
    )?;
    candidates.into_iter().find(|r| r.public == winner)
}

/// Serve one authenticated inbound connection as the relay: run sync rounds over
/// the shared durable store until the peer disconnects. The PostOffice ingests
/// (validates + persists) pushed events and serves held events to pullers; it
/// never decrypts. Generic over the IO so it is testable over an in-memory duplex.
#[cfg(test)]
pub async fn serve_relay_connection<IO>(channel: SecureChannel<IO>, store: Arc<Mutex<PostOffice>>)
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    serve_relay_or_bridge_connection(channel, store, None).await;
}

async fn serve_relay_or_bridge_connection<IO>(
    mut channel: SecureChannel<IO>,
    store: Arc<Mutex<PostOffice>>,
    directory: Option<Arc<Mutex<BridgeDirectory>>>,
) where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let first = match tokio::time::timeout(RELAY_IDLE_TIMEOUT, channel.recv()).await {
        Ok(Ok(bytes)) => bytes,
        _ => return,
    };
    if bridge_directory::is_request(&first) {
        if let Some(directory) = directory {
            let response = {
                let peers = directory
                    .lock()
                    .expect("bridge directory lock not poisoned")
                    .register(
                        channel.peer_identity(),
                        channel.peer_announcement(),
                        channel.peer_publicly_visible(),
                        std::time::Instant::now(),
                    );
                peers.and_then(|peers| bridge_directory::encode_response(&peers))
            };
            if let Some(response) = response {
                for page in response {
                    if !matches!(
                        tokio::time::timeout(RELAY_IDLE_TIMEOUT, channel.send(&page)).await,
                        Ok(Ok(()))
                    ) {
                        break;
                    }
                }
            }
        }
        return;
    }
    if !matches!(
        tokio::time::timeout(
            RELAY_IDLE_TIMEOUT,
            serve_wire_bytes(&mut channel, &store, &first)
        )
        .await,
        Ok(Ok(Served::Handled(_)))
    ) {
        return;
    }
    // Bound the connection so a buggy/malicious peer can't pin the always-on relay: cap
    // the number of sync rounds (a real reconciliation converges well under this) and
    // drop the connection if the peer goes idle mid-stream.
    for _ in 1..MAX_RELAY_ROUNDS {
        match tokio::time::timeout(RELAY_IDLE_TIMEOUT, serve_one(&mut channel, &store)).await {
            Ok(Ok(Served::Handled(_))) => continue,
            // peer closed, transport/protocol error, or idle past the timeout
            _ => break,
        }
    }
}

/// Per-connection round ceiling (matches the requester-side `MAX_SYNC_ROUNDS`).
const MAX_RELAY_ROUNDS: usize = 10_000;
/// Drop a relay connection that sends nothing for this long.
const RELAY_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Ceiling on concurrently-served relay connections, so a flood of TCP + Noise handshakes
/// can't spawn unbounded tasks on the always-on relay. Generous for a LAN; further
/// connections wait for a slot (back-pressure) rather than piling up as tasks.
const MAX_CONCURRENT_RELAY_CONNS: usize = 256;

/// Accept inbound connections on `listener` and serve each as a relay connection
/// on its own task, sharing the one durable store. A failed handshake backs off
/// briefly (so a persistent accept error can't busy-spin) and keeps accepting.
/// Concurrent connections are capped (see `MAX_CONCURRENT_RELAY_CONNS`).
pub async fn run_relay_accept_loop(
    identity: DeviceIdentity,
    listener: TcpListener,
    store: Arc<Mutex<PostOffice>>,
) {
    run_relay_accept_loop_inner(identity, listener, store, None).await;
}

/// Run an opt-in bridge directory on the existing authenticated relay port.
/// Sync mailboxes remain available on the same listener.
pub async fn run_bridge_relay_accept_loop(
    identity: DeviceIdentity,
    listener: TcpListener,
    store: Arc<Mutex<PostOffice>>,
) {
    run_relay_accept_loop_inner(
        identity,
        listener,
        store,
        Some(Arc::new(Mutex::new(BridgeDirectory::default()))),
    )
    .await;
}

/// Bridge relay with one pinned remote post-office gateway. Only clients that
/// explicitly register with this directory receive the remote relay proof.
pub async fn run_bridge_relay_accept_loop_with_gateway(
    identity: DeviceIdentity,
    listener: TcpListener,
    store: Arc<Mutex<PostOffice>>,
    gateway: Arc<Mutex<Option<Announce>>>,
) {
    run_relay_accept_loop_inner(
        identity,
        listener,
        store,
        Some(Arc::new(Mutex::new(BridgeDirectory::with_gateway(gateway)))),
    )
    .await;
}

async fn run_relay_accept_loop_inner(
    identity: DeviceIdentity,
    listener: TcpListener,
    store: Arc<Mutex<PostOffice>>,
    directory: Option<Arc<Mutex<BridgeDirectory>>>,
) {
    store
        .lock()
        .expect("relay store lock not poisoned")
        .require_opaque_envelopes();
    let conns = Arc::new(Semaphore::new(MAX_CONCURRENT_RELAY_CONNS));
    let identity = Arc::new(identity);
    loop {
        // Reserve a connection slot BEFORE accepting, so we never serve more than the cap;
        // excess inbound connections wait in the OS accept queue until a slot frees.
        let permit = match Arc::clone(&conns).acquire_owned().await {
            Ok(p) => p,
            Err(_) => return, // semaphore closed — shouldn't happen, but stop cleanly
        };
        // Only the (fast) TCP accept runs on the loop; the Noise handshake runs in the
        // spawned task (bounded by HANDSHAKE_TIMEOUT), so a peer that connects then stalls
        // mid-handshake can't park the loop and block the always-on relay's accept capacity.
        let stream = match listener.accept().await {
            Ok((stream, _addr)) => stream,
            Err(_) => {
                drop(permit);
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let store = Arc::clone(&store);
        let identity = Arc::clone(&identity);
        let directory = directory.clone();
        tokio::spawn(async move {
            let _permit = permit; // held for the connection's lifetime, freed on drop
            if let Ok(channel) = secure_accept(stream, &identity).await {
                serve_relay_or_bridge_connection(channel, store, directory).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::announce::Announce;
    use crate::eventlog::event::{ConversationId, Event, EventKind};
    use crate::eventlog::store::EventLog;
    use crate::identity::account::Account;
    use crate::node::session::request_round;
    use std::net::{IpAddr, Ipv4Addr};

    fn po_roster(relays: &[(&DeviceIdentity, u16)], normals: &[&DeviceIdentity]) -> Roster {
        let mut roster = Roster::default();
        for (id, port) in relays {
            roster.update(
                &Announce::new_post_office(id, "Relay", *port),
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                "self",
            );
        }
        for id in normals {
            roster.update(
                &Announce::new(id, "Node", 4000),
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                "self",
            );
        }
        roster
    }

    #[test]
    fn elected_post_office_is_none_without_a_po() {
        let alice = DeviceIdentity::generate();
        let roster = po_roster(&[], &[&alice]);
        assert!(elected_post_office(&roster).is_none());
    }

    #[test]
    fn elected_post_office_picks_the_lowest_fingerprint_po() {
        // Generate three PO identities; the winner is the lowest user_id().
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let c = DeviceIdentity::generate();
        let normal = DeviceIdentity::generate();
        let roster = po_roster(&[(&a, 5001), (&b, 5002), (&c, 5003)], &[&normal]);

        let mut ids = [a.public(), b.public(), c.public()];
        ids.sort_by_key(|p| p.user_id());
        let expected_lowest = ids[0].clone();

        let elected = elected_post_office(&roster).expect("a PO is elected");
        assert_eq!(elected.public, expected_lowest);
        assert!(elected.post_office);
    }

    #[tokio::test]
    async fn relay_ingests_an_event_pushed_over_a_channel() {
        // A client with one event pushes it to a relay (PostOffice) over a
        // SecureChannel on an in-memory duplex; the relay persists it.
        let dir = tempfile::tempdir().unwrap();
        let client_id = DeviceIdentity::generate();
        let relay_id = DeviceIdentity::generate();
        // The relay needs an identity copy: one for the PostOffice, one for Noise.
        let (ed, x) = relay_id.secret_bytes();
        let relay_transport_id = DeviceIdentity::from_secret_bytes(ed, x);

        let conv = ConversationId::new([7u8; 32]);
        let event = Event::new(
            &client_id,
            conv,
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"x".to_vec(),
        );
        let event_id = event.id;
        let client_store = Mutex::new({
            let mut log = EventLog::default();
            log.append(event.clone()).unwrap();
            log
        });

        let relay_store = Arc::new(Mutex::new(
            PostOffice::open(&dir.path().join("relay.log"), "pw", relay_id).unwrap(),
        ));

        let (c_io, r_io) = tokio::io::duplex(64 * 1024);
        let server_store = Arc::clone(&relay_store);
        let server = tokio::spawn(async move {
            let r_ch = SecureChannel::accept(r_io, &relay_transport_id)
                .await
                .unwrap();
            serve_relay_connection(r_ch, server_store).await;
        });

        let mut c_ch = SecureChannel::connect(c_io, &client_id, None)
            .await
            .unwrap();
        request_round(&mut c_ch, &client_store, conv).await.unwrap();
        drop(c_ch); // hang up so the relay's serve loop ends and the task joins
        server.await.unwrap();

        assert!(
            relay_store.lock().unwrap().has(&event_id),
            "relay persisted the pushed event"
        );
    }

    #[tokio::test]
    async fn bridge_directory_introduces_only_authenticated_opted_in_devices() {
        let root = tempfile::tempdir().unwrap();
        let relay = DeviceIdentity::generate();
        let relay_keys = relay.secret_bytes();
        let store = Arc::new(Mutex::new(
            PostOffice::open(&root.path().join("relay.log"), "pw", relay).unwrap(),
        ));
        let directory = Arc::new(Mutex::new(BridgeDirectory::default()));
        let account = Account::generate();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let alice_proof = Announce::new_with_account(&alice, &account, "Alice", 4001);
        let bob_proof = Announce::new_with_account(&bob, &account, "Bob", 4002);

        for (client, proof, expected) in [
            (&alice, &alice_proof, None),
            (&bob, &bob_proof, Some(alice_proof.user_id.as_str())),
        ] {
            let (client_io, relay_io) = tokio::io::duplex(64 * 1024);
            let identity = DeviceIdentity::from_secret_bytes(relay_keys.0, relay_keys.1);
            let relay_store = Arc::clone(&store);
            let relay_directory = Arc::clone(&directory);
            let server = tokio::spawn(async move {
                let channel = SecureChannel::accept(relay_io, &identity).await.unwrap();
                serve_relay_or_bridge_connection(channel, relay_store, Some(relay_directory)).await;
            });
            let mut channel =
                SecureChannel::connect_with_presence(client_io, client, None, Some(proof))
                    .await
                    .unwrap();
            channel.send(bridge_directory::request()).await.unwrap();
            let response = channel.recv().await.unwrap();
            let (peers, last) = bridge_directory::decode_response(&response).unwrap();
            assert!(last);
            match expected {
                None => assert!(peers.is_empty()),
                Some(id) => assert_eq!(
                    peers.iter().map(|p| p.user_id.as_str()).collect::<Vec<_>>(),
                    vec![id]
                ),
            }
            drop(channel);
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn bridge_listener_returns_a_pinned_gateway_proof_over_noise() {
        let root = tempfile::tempdir().unwrap();
        let relay = DeviceIdentity::generate();
        let relay_keys = relay.secret_bytes();
        let store = Arc::new(Mutex::new(
            PostOffice::open(
                &root.path().join("relay.log"),
                "pw",
                DeviceIdentity::from_secret_bytes(relay_keys.0, relay_keys.1),
            )
            .unwrap(),
        ));
        let remote = DeviceIdentity::generate();
        let remote_account = Account::generate();
        let remote_proof =
            Announce::new_post_office_with_account(&remote, &remote_account, "remote", 47475);
        let gateway = Arc::new(Mutex::new(Some(remote_proof.clone())));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(run_bridge_relay_accept_loop_with_gateway(
            relay, listener, store, gateway,
        ));

        let client = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = Announce::new_with_account(&client, &account, "client", 47500);
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut channel = SecureChannel::connect_with_presence(stream, &client, None, Some(&proof))
            .await
            .unwrap();
        channel.send(bridge_directory::request()).await.unwrap();
        let response = tokio::time::timeout(Duration::from_secs(3), channel.recv())
            .await
            .unwrap()
            .unwrap();
        let (peers, last) = bridge_directory::decode_response(&response).unwrap();
        assert!(last);
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].user_id, remote_proof.user_id);
        drop(channel);
        server.abort();
    }

    #[tokio::test]
    async fn production_relay_rejects_clear_event_headers() {
        let dir = tempfile::tempdir().unwrap();
        let sender = DeviceIdentity::generate();
        let relay = DeviceIdentity::generate();
        let relay_keys = relay.secret_bytes();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let store = Arc::new(Mutex::new(
            PostOffice::open(&dir.path().join("relay.log"), "pw", relay).unwrap(),
        ));
        let task = tokio::spawn(run_relay_accept_loop(
            DeviceIdentity::from_secret_bytes(relay_keys.0, relay_keys.1),
            listener,
            Arc::clone(&store),
        ));
        let event = Event::new(
            &sender,
            ConversationId::new([8; 32]),
            1,
            vec![],
            1,
            1234,
            EventKind::MembershipChange,
            b"readable channel name".to_vec(),
        );
        let mut log = EventLog::default();
        log.append(event.clone()).unwrap();
        let mut channel = SecureChannel::connect(
            tokio::net::TcpStream::connect(addr).await.unwrap(),
            &sender,
            None,
        )
        .await
        .unwrap();
        let _ = request_round(&mut channel, &Mutex::new(log), event.conversation_id).await;
        assert!(!store.lock().unwrap().has(&event.id));
        task.abort();
    }

    #[test]
    fn wrapped_file_chunk_fits_one_sync_frame() {
        let sender = DeviceIdentity::generate();
        let recipient = DeviceIdentity::generate();
        let relay = DeviceIdentity::generate();
        let chunk = Event::new(
            &sender,
            ConversationId::new([6; 32]),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![7; crate::file::crypto::CHUNK_SIZE + 16],
        );
        let mailbox = crate::postoffice::opaque::RelayMailbox::new(
            &sender,
            &recipient.public(),
            &relay.public(),
        )
        .unwrap();
        assert!(super::super::session::event_fits_frame(
            &mailbox.wrap(&chunk).unwrap()
        ));
    }
}
