//! Post-office integration: find the elected post office in the roster, and run
//! the relay's server side (sync rounds over a durable [`PostOffice`]). The
//! Client upload and drain live in `opaque_relay` and `serving`;
//! this module provides election lookup and the serving loop.

use crate::discovery::roster::{PeerRecord, Roster, RouteSource};
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::node::bridge_directory::{self, BridgeDirectory};
use crate::node::session::{Served, serve_one, serve_wire_bytes};
use crate::node::transport::secure_accept;
use crate::postoffice::federation;
use crate::postoffice::gateway::GatewayProof;
use crate::postoffice::{PostOffice, elect};
use crate::transport::SecureChannel;
use std::collections::HashSet;
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

/// Order verified post offices for delivery. A configured ID is a preference,
/// never an exclusive mailbox. Without one, use a directly discovered LAN
/// relay before a remote gateway; break ties by fingerprint for stability.
pub fn post_office_candidates(roster: &Roster, preferred_id: Option<&str>) -> Vec<PeerRecord> {
    let mut candidates = roster.post_offices();
    candidates.sort_by_key(|relay| {
        (
            preferred_id != Some(relay.public.user_id().as_str()),
            !relay.has_lan_route(),
            relay.public.user_id(),
        )
    });
    candidates
}

/// The first known post office in preference order. Callers performing network
/// I/O should try subsequent candidates if this relay is unreachable.
pub fn selected_post_office(roster: &Roster, hub_id: Option<&str>) -> Option<PeerRecord> {
    post_office_candidates(roster, hub_id).into_iter().next()
}

/// For an introduced recipient, favor the relays that actually reported that
/// recipient. A merely local relay may have no route to the other site's
/// client, so successful custody there would not imply deliverability.
pub fn post_office_candidates_for(
    roster: &Roster,
    preferred_id: Option<&str>,
    recipient: &PublicIdentity,
) -> Vec<PeerRecord> {
    let mut candidates = post_office_candidates(roster, preferred_id);
    if let Some(peer) = roster
        .get(&recipient.user_id())
        .filter(|peer| peer.public == *recipient)
    {
        let introduced_by: Vec<_> = peer
            .routes
            .iter()
            .filter(|route| route.source == RouteSource::PostOffice)
            .map(|route| route.addr)
            .collect();
        if !introduced_by.is_empty() {
            candidates.sort_by_key(|relay| !introduced_by.contains(&relay.addr));
        }
    }
    candidates
}

impl super::Node {
    pub fn set_bridge_hub_id(&self, id: Option<&str>) -> Result<(), &'static str> {
        if id
            .is_some_and(|value| value.len() != 32 || !value.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("bridge hub ID must be a full 32-character hexadecimal fingerprint");
        }
        let new_id = id.map(str::to_ascii_lowercase);
        let changed = {
            let mut hub = self
                .bridge_hub_id
                .lock()
                .expect("bridge hub lock not poisoned");
            let changed = *hub != new_id;
            *hub = new_id;
            changed
        };
        if changed {
            self.clear_bridge_introductions();
        }
        self.bridge_notify.notify_one();
        self.delivery_notify.notify_one();
        Ok(())
    }

    pub(in crate::node) fn selected_post_office(&self, roster: &Roster) -> Option<PeerRecord> {
        let hub = self
            .bridge_hub_id
            .lock()
            .expect("bridge hub lock not poisoned");
        selected_post_office(roster, hub.as_deref())
    }

    pub(in crate::node) fn post_office_candidates(&self, roster: &Roster) -> Vec<PeerRecord> {
        let preferred = self
            .bridge_hub_id
            .lock()
            .expect("bridge hub lock not poisoned");
        post_office_candidates(roster, preferred.as_deref())
    }

    pub(in crate::node) fn post_office_candidates_for(
        &self,
        roster: &Roster,
        recipient: &PublicIdentity,
    ) -> Vec<PeerRecord> {
        let preferred = self
            .bridge_hub_id
            .lock()
            .expect("bridge hub lock not poisoned");
        post_office_candidates_for(roster, preferred.as_deref(), recipient)
    }
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

#[cfg(test)]
async fn serve_relay_or_bridge_connection<IO>(
    channel: SecureChannel<IO>,
    store: Arc<Mutex<PostOffice>>,
    directory: Option<Arc<Mutex<BridgeDirectory>>>,
) where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    serve_relay_or_bridge_connection_with_trust(channel, store, directory, None).await;
}

async fn serve_relay_or_bridge_connection_with_trust<IO>(
    mut channel: SecureChannel<IO>,
    store: Arc<Mutex<PostOffice>>,
    directory: Option<Arc<Mutex<BridgeDirectory>>>,
    federation_trust: Option<Arc<HashSet<String>>>,
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
                let mut directory = directory
                    .lock()
                    .expect("bridge directory lock not poisoned");
                let peers = directory.register(
                    channel.peer_identity(),
                    channel.peer_announcement(),
                    channel.peer_publicly_visible(),
                    std::time::Instant::now(),
                );
                peers.and_then(|peers| {
                    if first == bridge_directory::legacy_request() {
                        bridge_directory::encode_response(&peers)
                    } else {
                        bridge_directory::encode_response_with_gateway(
                            &peers,
                            directory.gateway_route(),
                        )
                    }
                })
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
    if federation::is_inventory_request(&first) {
        if !federation_trust
            .as_ref()
            .is_some_and(|trusted| trusted.contains(&channel.peer_identity().user_id()))
            || federation::send_inventory(&mut channel, &store, directory.as_ref())
                .await
                .is_err()
        {
            return;
        }
        for _ in 0..MAX_RELAY_ROUNDS {
            match tokio::time::timeout(RELAY_IDLE_TIMEOUT, serve_one(&mut channel, &store)).await {
                Ok(Ok(Served::Handled(_))) => continue,
                _ => break,
            }
        }
        return;
    }
    if let Err(error) = store
        .lock()
        .expect("post office lock not poisoned")
        .prune_expired()
    {
        log::warn!("post office retention cleanup failed: {error}");
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
    gateway: Arc<Mutex<Option<GatewayProof>>>,
) {
    run_relay_accept_loop_inner(
        identity,
        listener,
        store,
        Some(Arc::new(Mutex::new(BridgeDirectory::with_gateway(gateway)))),
    )
    .await;
}

/// Serve normal clients and an equal-relay federation on the same listener.
/// Inventory and bulk opaque replication are restricted to pinned identities.
#[cfg(test)]
pub async fn run_federated_relay_accept_loop(
    identity: DeviceIdentity,
    listener: TcpListener,
    store: Arc<Mutex<PostOffice>>,
    trusted_relays: Arc<HashSet<String>>,
) {
    run_federated_relay_accept_loop_with_directory(
        identity,
        listener,
        store,
        trusted_relays,
        federation::FederationDirectory::new(),
    )
    .await;
}

pub async fn run_federated_relay_accept_loop_with_directory(
    identity: DeviceIdentity,
    listener: TcpListener,
    store: Arc<Mutex<PostOffice>>,
    trusted_relays: Arc<HashSet<String>>,
    directory: federation::FederationDirectory,
) {
    run_relay_accept_loop_inner_with_trust(
        identity,
        listener,
        store,
        Some(directory.shared()),
        Some(trusted_relays),
    )
    .await;
}

async fn run_relay_accept_loop_inner(
    identity: DeviceIdentity,
    listener: TcpListener,
    store: Arc<Mutex<PostOffice>>,
    directory: Option<Arc<Mutex<BridgeDirectory>>>,
) {
    run_relay_accept_loop_inner_with_trust(identity, listener, store, directory, None).await;
}

async fn run_relay_accept_loop_inner_with_trust(
    identity: DeviceIdentity,
    listener: TcpListener,
    store: Arc<Mutex<PostOffice>>,
    directory: Option<Arc<Mutex<BridgeDirectory>>>,
    federation_trust: Option<Arc<HashSet<String>>>,
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
        let federation_trust = federation_trust.clone();
        tokio::spawn(async move {
            let _permit = permit; // held for the connection's lifetime, freed on drop
            if let Ok(channel) = secure_accept(stream, &identity).await {
                serve_relay_or_bridge_connection_with_trust(
                    channel,
                    store,
                    directory,
                    federation_trust,
                )
                .await;
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

    #[test]
    fn configured_relay_is_preferred_with_mailbox_fallback() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let (local, hub) = if a.user_id() < b.user_id() {
            (&a, &b)
        } else {
            (&b, &a)
        };
        let roster = po_roster(&[(local, 5001), (hub, 5002)], &[]);
        assert_eq!(
            selected_post_office(&roster, Some(&hub.user_id()))
                .unwrap()
                .public,
            hub.public()
        );
        assert_eq!(
            selected_post_office(&po_roster(&[(local, 5001)], &[]), Some(&hub.user_id()))
                .unwrap()
                .public,
            local.public()
        );
    }

    #[test]
    fn unconfigured_client_prefers_its_local_relay_over_a_gateway() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let (remote, local) = if a.user_id() < b.user_id() {
            (&a, &b)
        } else {
            (&b, &a)
        };
        let account = crate::identity::account::Account::generate();
        let mut roster = Roster::default();
        roster.update_gateway(
            &crate::discovery::Announce::new_post_office_with_account(
                remote, &account, "remote", 47475,
            ),
            "192.168.0.99".parse().unwrap(),
            47477,
            "self",
        );
        roster.update(
            &crate::discovery::Announce::new_post_office_with_account(
                local, &account, "local", 47476,
            ),
            "192.168.0.99".parse().unwrap(),
            "self",
        );
        assert_eq!(
            selected_post_office(&roster, None).unwrap().public,
            local.public()
        );
        assert_eq!(
            selected_post_office(&roster, Some(&remote.user_id()))
                .unwrap()
                .public,
            remote.public()
        );
    }

    #[test]
    fn recipient_introduction_routes_mail_to_a_shared_relay() {
        let local = DeviceIdentity::generate();
        let shared = DeviceIdentity::generate();
        let recipient = DeviceIdentity::generate();
        let account = crate::identity::account::Account::generate();
        let mut roster = Roster::default();
        roster.update(
            &crate::discovery::Announce::new_post_office_with_account(
                &local, &account, "local", 47475,
            ),
            "192.168.3.16".parse().unwrap(),
            "self",
        );
        roster.update(
            &crate::discovery::Announce::new_post_office_with_account(
                &shared, &account, "shared", 47476,
            ),
            "192.168.3.72".parse().unwrap(),
            "self",
        );
        roster.update_relayed(
            &crate::discovery::Announce::new_with_account(&recipient, &account, "remote", 55000),
            "192.168.3.72:47476".parse().unwrap(),
            "self",
        );
        assert_eq!(
            post_office_candidates_for(&roster, Some(&local.user_id()), &recipient.public())[0]
                .public,
            shared.public()
        );
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
    async fn trusted_relays_reconcile_federated_mailboxes_after_disconnect() {
        let root = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let first = DeviceIdentity::generate();
        let second = DeviceIdentity::generate();
        let first_keys = first.secret_bytes();
        let second_keys = second.secret_bytes();
        let first_public = first.public();
        let second_public = second.public();
        let first_store = Arc::new(Mutex::new(
            PostOffice::open(&root.path().join("first"), "pw", first).unwrap(),
        ));
        let second_store = Arc::new(Mutex::new(
            PostOffice::open(&root.path().join("second"), "pw", second).unwrap(),
        ));
        let mailbox =
            crate::postoffice::opaque::RelayMailbox::federated(&alice, &bob.public()).unwrap();
        let legacy =
            crate::postoffice::opaque::RelayMailbox::new(&alice, &bob.public(), &first_public)
                .unwrap();
        let event = Event::new(
            &alice,
            ConversationId::new([61; 32]),
            1,
            vec![],
            1,
            10,
            EventKind::Message,
            b"opaque".to_vec(),
        );
        let outer = mailbox.wrap(&event).unwrap();
        let legacy_outer = legacy.wrap(&event).unwrap();
        first_store.lock().unwrap().accept(outer.clone()).unwrap();
        first_store
            .lock()
            .unwrap()
            .accept(legacy_outer.clone())
            .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let trusted = Arc::new(std::collections::HashSet::from([first_public.user_id()]));
        let second_server = tokio::spawn(run_federated_relay_accept_loop(
            DeviceIdentity::from_secret_bytes(second_keys.0, second_keys.1),
            listener,
            Arc::clone(&second_store),
            trusted,
        ));
        crate::postoffice::federation::sync_with_relay(
            addr,
            &DeviceIdentity::from_secret_bytes(first_keys.0, first_keys.1),
            &second_public,
            &first_store,
        )
        .await
        .unwrap();
        assert!(second_store.lock().unwrap().has(&outer.id));
        assert!(!second_store.lock().unwrap().has(&legacy_outer.id));
        assert_eq!(
            crate::postoffice::federation::sync_with_relay(
                addr,
                &DeviceIdentity::from_secret_bytes(first_keys.0, first_keys.1),
                &second_public,
                &first_store,
            )
            .await
            .unwrap(),
            0,
            "matching mailbox digests should avoid a redundant sync round",
        );

        second_server.abort();
        let _ = second_server.await;
        let reply = Event::new(
            &bob,
            ConversationId::new([62; 32]),
            1,
            vec![],
            1,
            11,
            EventKind::Message,
            b"reply".to_vec(),
        );
        let reply_outer = crate::postoffice::opaque::RelayMailbox::federated(&bob, &alice.public())
            .unwrap()
            .wrap(&reply)
            .unwrap();
        second_store
            .lock()
            .unwrap()
            .accept(reply_outer.clone())
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let recovered_addr = listener.local_addr().unwrap();
        let trusted = Arc::new(std::collections::HashSet::from([first_public.user_id()]));
        let second_server = tokio::spawn(run_federated_relay_accept_loop(
            DeviceIdentity::from_secret_bytes(second_keys.0, second_keys.1),
            listener,
            Arc::clone(&second_store),
            trusted,
        ));
        crate::postoffice::federation::sync_with_relay(
            recovered_addr,
            &DeviceIdentity::from_secret_bytes(first_keys.0, first_keys.1),
            &second_public,
            &first_store,
        )
        .await
        .unwrap();
        assert!(first_store.lock().unwrap().has(&reply_outer.id));
        second_server.abort();
    }

    #[tokio::test]
    async fn trusted_relays_exchange_only_opted_in_signed_presence() {
        let root = tempfile::tempdir().unwrap();
        let first = DeviceIdentity::generate();
        let second = DeviceIdentity::generate();
        let first_keys = first.secret_bytes();
        let second_keys = second.secret_bytes();
        let first_public = first.public();
        let second_public = second.public();
        let first_store = Arc::new(Mutex::new(
            PostOffice::open(&root.path().join("first"), "pw", first).unwrap(),
        ));
        let second_store = Arc::new(Mutex::new(
            PostOffice::open(&root.path().join("second"), "pw", second).unwrap(),
        ));
        let first_directory = federation::FederationDirectory::new();
        let second_directory = federation::FederationDirectory::new();
        let account = Account::generate();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let hidden = DeviceIdentity::generate();
        let alice_proof = Announce::new_with_account(&alice, &account, "Alice", 47475);
        let bob_proof = Announce::new_with_account(&bob, &account, "Bob", 47476);
        let hidden_proof = Announce::new_with_account(&hidden, &account, "Hidden", 47477);
        let now = std::time::Instant::now();
        first_directory
            .shared()
            .lock()
            .unwrap()
            .register(&alice.public(), Some(&alice_proof), true, now)
            .unwrap();
        first_directory.shared().lock().unwrap().register(
            &hidden.public(),
            Some(&hidden_proof),
            false,
            now,
        );
        second_directory
            .shared()
            .lock()
            .unwrap()
            .register(&bob.public(), Some(&bob_proof), true, now)
            .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(run_federated_relay_accept_loop_with_directory(
            DeviceIdentity::from_secret_bytes(second_keys.0, second_keys.1),
            listener,
            second_store,
            Arc::new(HashSet::from([first_public.user_id()])),
            second_directory.clone(),
        ));
        first_directory
            .sync_with_relay(
                addr,
                &DeviceIdentity::from_secret_bytes(first_keys.0, first_keys.1),
                &second_public,
                &first_store,
            )
            .await
            .unwrap();
        let observer = DeviceIdentity::generate();
        let observer_proof = Announce::new_with_account(&observer, &account, "Observer", 47478);
        let proofs = second_directory
            .shared()
            .lock()
            .unwrap()
            .register(&observer.public(), Some(&observer_proof), true, now)
            .unwrap();
        assert!(proofs.iter().any(|proof| proof.user_id == alice.user_id()));
        assert!(!proofs.iter().any(|proof| proof.user_id == hidden.user_id()));
        assert!(
            !proofs
                .iter()
                .any(|proof| proof.user_id == first_public.user_id())
        );
        server.abort();
    }

    #[tokio::test]
    async fn untrusted_relay_cannot_read_federation_inventory() {
        let root = tempfile::tempdir().unwrap();
        let server_identity = DeviceIdentity::generate();
        let server_keys = server_identity.secret_bytes();
        let server_public = server_identity.public();
        let client_identity = DeviceIdentity::generate();
        let client_store = Arc::new(Mutex::new(
            PostOffice::open(
                &root.path().join("client"),
                "pw",
                DeviceIdentity::generate(),
            )
            .unwrap(),
        ));
        let server_store = Arc::new(Mutex::new(
            PostOffice::open(&root.path().join("server"), "pw", server_identity).unwrap(),
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(run_federated_relay_accept_loop(
            DeviceIdentity::from_secret_bytes(server_keys.0, server_keys.1),
            listener,
            server_store,
            Arc::new(HashSet::new()),
        ));
        assert!(
            federation::sync_with_relay(addr, &client_identity, &server_public, &client_store)
                .await
                .is_err()
        );
        server.abort();
    }

    #[tokio::test]
    async fn three_equal_relays_forward_after_middle_route_recovers() {
        let root = tempfile::tempdir().unwrap();
        let sender = DeviceIdentity::generate();
        let recipient = DeviceIdentity::generate();
        let mailbox =
            crate::postoffice::opaque::RelayMailbox::federated(&sender, &recipient.public())
                .unwrap();
        let west = DeviceIdentity::generate();
        let middle = DeviceIdentity::generate();
        let east = DeviceIdentity::generate();
        let west_keys = west.secret_bytes();
        let middle_keys = middle.secret_bytes();
        let east_keys = east.secret_bytes();
        let west_public = west.public();
        let middle_public = middle.public();
        let east_public = east.public();
        let west_store = Arc::new(Mutex::new(
            PostOffice::open(&root.path().join("west"), "pw", west).unwrap(),
        ));
        let middle_store = Arc::new(Mutex::new(
            PostOffice::open(&root.path().join("middle"), "pw", middle).unwrap(),
        ));
        let east_store = Arc::new(Mutex::new(
            PostOffice::open(&root.path().join("east"), "pw", east).unwrap(),
        ));
        let middle_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let middle_addr = middle_listener.local_addr().unwrap();
        let east_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let east_addr = east_listener.local_addr().unwrap();
        let middle_server = tokio::spawn(run_federated_relay_accept_loop(
            DeviceIdentity::from_secret_bytes(middle_keys.0, middle_keys.1),
            middle_listener,
            Arc::clone(&middle_store),
            Arc::new(HashSet::from([
                west_public.user_id(),
                east_public.user_id(),
            ])),
        ));
        let east_server = tokio::spawn(run_federated_relay_accept_loop(
            DeviceIdentity::from_secret_bytes(east_keys.0, east_keys.1),
            east_listener,
            Arc::clone(&east_store),
            Arc::new(HashSet::from([middle_public.user_id()])),
        ));
        let first = mailbox
            .wrap(&Event::new(
                &sender,
                ConversationId::new([71; 32]),
                1,
                vec![],
                1,
                10,
                EventKind::Message,
                b"first".to_vec(),
            ))
            .unwrap();
        west_store.lock().unwrap().accept(first.clone()).unwrap();
        federation::sync_with_relay(
            middle_addr,
            &DeviceIdentity::from_secret_bytes(west_keys.0, west_keys.1),
            &middle_public,
            &west_store,
        )
        .await
        .unwrap();
        federation::sync_with_relay(
            east_addr,
            &DeviceIdentity::from_secret_bytes(middle_keys.0, middle_keys.1),
            &east_public,
            &middle_store,
        )
        .await
        .unwrap();
        assert!(east_store.lock().unwrap().has(&first.id));

        middle_server.abort();
        let _ = middle_server.await;
        let second = mailbox
            .wrap(&Event::new(
                &sender,
                ConversationId::new([72; 32]),
                1,
                vec![],
                1,
                11,
                EventKind::Message,
                b"queued".to_vec(),
            ))
            .unwrap();
        west_store.lock().unwrap().accept(second.clone()).unwrap();
        assert!(
            federation::sync_with_relay(
                middle_addr,
                &DeviceIdentity::from_secret_bytes(west_keys.0, west_keys.1),
                &middle_public,
                &west_store
            )
            .await
            .is_err()
        );
        assert!(!east_store.lock().unwrap().has(&second.id));
        let middle_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let recovered_addr = middle_listener.local_addr().unwrap();
        let recovered = tokio::spawn(run_federated_relay_accept_loop(
            DeviceIdentity::from_secret_bytes(middle_keys.0, middle_keys.1),
            middle_listener,
            Arc::clone(&middle_store),
            Arc::new(HashSet::from([
                west_public.user_id(),
                east_public.user_id(),
            ])),
        ));
        federation::sync_with_relay(
            recovered_addr,
            &DeviceIdentity::from_secret_bytes(west_keys.0, west_keys.1),
            &middle_public,
            &west_store,
        )
        .await
        .unwrap();
        federation::sync_with_relay(
            east_addr,
            &DeviceIdentity::from_secret_bytes(middle_keys.0, middle_keys.1),
            &east_public,
            &middle_store,
        )
        .await
        .unwrap();
        assert!(east_store.lock().unwrap().has(&second.id));
        recovered.abort();
        east_server.abort();
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

        for (client, proof, expected, legacy) in [
            (&alice, &alice_proof, None, false),
            (&bob, &bob_proof, Some(alice_proof.user_id.as_str()), false),
            (&bob, &bob_proof, Some(alice_proof.user_id.as_str()), true),
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
            let request = if legacy {
                bridge_directory::legacy_request()
            } else {
                bridge_directory::request()
            };
            channel.send(request).await.unwrap();
            let response = channel.recv().await.unwrap();
            let (peers, gateway, last) = if legacy {
                let (peers, last) = bridge_directory::decode_response(&response).unwrap();
                (peers, None, last)
            } else {
                bridge_directory::decode_response_with_gateway(&response).unwrap()
            };
            assert!(last);
            assert!(gateway.is_none());
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
        let gateway = Arc::new(Mutex::new(Some(GatewayProof {
            announce: remote_proof.clone(),
            proxy_port: 50000,
        })));
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
        let (peers, gateway, last) =
            bridge_directory::decode_response_with_gateway(&response).unwrap();
        assert!(last);
        assert_eq!(gateway, Some((remote_proof.user_id.clone(), 50000)));
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
