//! Bounded relay reconciliation driven by explicit public trust pins.
use super::federation_config::FederationSeed;
use futures_util::{StreamExt, future, stream};
use mesh_talk_core::discovery::Roster;
use mesh_talk_core::identity::device::DeviceIdentity;
use mesh_talk_core::postoffice::PostOffice;
use mesh_talk_core::postoffice::federation::FederationDirectory;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// Independent operation budgets, not a global socket cap or delivery deadline.
const MAX_SEED_PROBES: usize = 8;
const MAX_RELAY_SYNCS: usize = 8;

pub(super) async fn run_federation_loop(
    roster: Arc<Mutex<Roster>>,
    seeds: Vec<FederationSeed>,
    identity: DeviceIdentity,
    store: Arc<Mutex<PostOffice>>,
    directory: FederationDirectory,
) {
    loop {
        federation_round(&roster, &seeds, &identity, &store, &directory).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn federation_round(
    roster: &Mutex<Roster>,
    seeds: &[FederationSeed],
    identity: &DeviceIdentity,
    store: &Arc<Mutex<PostOffice>>,
    directory: &FederationDirectory,
) {
    let self_id = identity.user_id();
    let trusted = seeds
        .iter()
        .map(|seed| seed.id.as_str())
        .collect::<HashSet<_>>();
    // Snapshot public routes and release the roster guard before any network await.
    let known = {
        roster
            .lock()
            .expect("roster lock not poisoned")
            .post_offices()
            .into_iter()
            .filter(|peer| {
                peer.public.user_id() != self_id && trusted.contains(peer.public.user_id().as_str())
            })
            .map(|peer| (peer.addr, peer.public))
            .collect::<Vec<_>>()
    };
    let routed_seeds = seeds
        .iter()
        .filter(|seed| seed.id != self_id)
        .cloned()
        .collect::<Vec<_>>();
    let probed = stream::iter(routed_seeds)
        .map(|seed| async move {
            mesh_talk_core::postoffice::gateway::query_pinned_relay(seed.discovery, &seed.id)
                .await
                .map(|proof| {
                    (
                        SocketAddr::new(seed.discovery.ip(), proof.tcp_port),
                        proof.public(),
                    )
                })
        })
        .buffer_unordered(MAX_SEED_PROBES)
        .filter_map(future::ready);
    let mut seen = HashSet::new();
    // Offer every known LAN route first. Then consume successful routed proofs as
    // they finish, without waiting for a complete collection of slow probes.
    stream::iter(known)
        .chain(probed)
        .filter(move |(addr, peer)| {
            future::ready(seen.insert((*addr, peer.ed25519_pub, peer.x25519_pub)))
        })
        .for_each_concurrent(MAX_RELAY_SYNCS, |(addr, peer)| async move {
            if let Err(error) = directory
                .sync_with_relay(addr, identity, &peer, store)
                .await
            {
                log::debug!("federation {} at {addr}: {error}", peer.user_id());
            }
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_talk_core::discovery::Announce;
    use tokio::net::{TcpListener, UdpSocket};

    fn local_store(temp: &tempfile::TempDir) -> Arc<Mutex<PostOffice>> {
        Arc::new(Mutex::new(
            PostOffice::open(
                &temp.path().join("relay.log"),
                "pw",
                DeviceIdentity::generate(),
            )
            .unwrap(),
        ))
    }

    #[tokio::test]
    async fn reconciliation_opens_at_most_eight_noise_handshakes() {
        let temp = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let mut roster = Roster::default();
        let stalled_queries = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let (accepted_tx, mut accepted_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut listeners = Vec::new();
        let mut seeds = Vec::new();
        for _ in 0..9 {
            let peer = DeviceIdentity::generate();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            roster.update(
                &Announce::new_post_office(&peer, "relay", addr.port()),
                addr.ip(),
                &identity.user_id(),
            );
            seeds.push(FederationSeed {
                id: peer.user_id(),
                discovery: stalled_queries.local_addr().unwrap(),
            });
            let accepted = accepted_tx.clone();
            listeners.push(tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                accepted.send(stream).unwrap();
            }));
        }
        let runner = tokio::spawn(run_federation_loop(
            Arc::new(Mutex::new(roster)),
            seeds,
            identity,
            local_store(&temp),
            FederationDirectory::default(),
        ));
        let mut held = Vec::new();
        for _ in 0..8 {
            held.push(
                tokio::time::timeout(Duration::from_secs(6), accepted_rx.recv())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        let ninth = tokio::time::timeout(Duration::from_millis(200), accepted_rx.recv()).await;
        runner.abort();
        let _ = runner.await;
        for listener in listeners {
            listener.abort();
        }
        assert!(
            ninth.is_err(),
            "ninth Noise handshake started while eight remain stalled"
        );
    }

    #[tokio::test]
    async fn known_lan_route_starts_before_stalled_seed_query_finishes() {
        let temp = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let peer = DeviceIdentity::generate();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stalled_query = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut roster = Roster::default();
        roster.update(
            &Announce::new_post_office(&peer, "relay", addr.port()),
            addr.ip(),
            &identity.user_id(),
        );
        let runner = tokio::spawn(run_federation_loop(
            Arc::new(Mutex::new(roster)),
            vec![FederationSeed {
                id: peer.user_id(),
                discovery: stalled_query.local_addr().unwrap(),
            }],
            identity,
            local_store(&temp),
            FederationDirectory::default(),
        ));
        let connected = tokio::time::timeout(Duration::from_secs(1), listener.accept()).await;
        runner.abort();
        let _ = runner.await;
        assert!(
            connected.is_ok(),
            "known LAN route waited for the stalled UDP seed"
        );
    }

    #[tokio::test]
    async fn routed_seed_probes_have_at_most_eight_in_flight_queries() {
        let temp = tempfile::tempdir().unwrap();
        let queries = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let seeds = (0..9)
            .map(|_| FederationSeed {
                id: DeviceIdentity::generate().user_id(),
                discovery: queries.local_addr().unwrap(),
            })
            .collect();
        let runner = tokio::spawn(run_federation_loop(
            Arc::new(Mutex::new(Roster::default())),
            seeds,
            DeviceIdentity::generate(),
            local_store(&temp),
            FederationDirectory::default(),
        ));
        let mut bytes = [0u8; 2048];
        for _ in 0..8 {
            tokio::time::timeout(Duration::from_secs(1), queries.recv_from(&mut bytes))
                .await
                .unwrap()
                .unwrap();
        }
        let ninth =
            tokio::time::timeout(Duration::from_millis(200), queries.recv_from(&mut bytes)).await;
        runner.abort();
        let _ = runner.await;
        assert!(
            ninth.is_err(),
            "ninth UDP query started while eight remain pending"
        );
    }
}
