#![cfg(feature = "mesh-connect")]

use mesh_connect::PeerSession;
use mesh_replication::{
    CompleteInventory,
    inventory_session::{InventorySender, receive_inventory, send_inventory},
    pull::{PullLimits, Record, pull_over_session, serve_record_request},
    session::HaveLimits,
};
use sha2::{Digest, Sha256};
use std::convert::Infallible;
use tokio::net::{TcpListener, TcpStream};
use x25519_dalek::{PublicKey, StaticSecret};

fn public(secret: [u8; 32]) -> [u8; 32] {
    PublicKey::from(&StaticSecret::from(secret)).to_bytes()
}

fn inventory_limits() -> HaveLimits {
    HaveLimits {
        ids_per_frame: 1,
        frames: 1,
        total_ids: 1,
    }
}

const BODY_LIMITS: PullLimits = PullLimits {
    records_per_batch: 1,
    bytes_per_batch: 64,
};

#[tokio::test]
async fn default_inventory_and_record_exchange_over_authenticated_tcp() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let scope = [7; 32];
    let body = b"application-defined record";
    let id: [u8; 32] = Sha256::digest(body).into();
    let source = CompleteInventory::from_ids(&[id], 1).unwrap();
    let local = CompleteInventory::from_ids(&[], 1).unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(
            async {
                let stream = TcpStream::connect(address).await.unwrap();
                let mut peer = PeerSession::connect(stream, &[11; 32], &public([22; 32]))
                    .await
                    .unwrap();
                let remote = receive_inventory(
                    &mut peer,
                    scope,
                    InventorySender::Responder,
                    inventory_limits(),
                )
                .await
                .unwrap();
                let mut stored = None;
                let count =
                    pull_over_session(&mut peer, scope, &remote, &local, BODY_LIMITS, |records| {
                        let [record] = records else {
                            panic!("expected one record")
                        };
                        assert_eq!(<[u8; 32]>::from(Sha256::digest(&record.body)), record.id);
                        stored = Some(record.body.clone());
                        Ok::<_, Infallible>(())
                    })
                    .await
                    .unwrap();
                assert_eq!(count, 1);
                assert_eq!(stored.as_deref(), Some(body.as_slice()));
            },
            async {
                let (stream, _) = listener.accept().await.unwrap();
                let mut peer =
                    PeerSession::accept(stream, &[22; 32], |key| *key == public([11; 32]))
                        .await
                        .unwrap();
                send_inventory(
                    &mut peer,
                    scope,
                    InventorySender::Responder,
                    &source,
                    inventory_limits(),
                    || Ok(()),
                )
                .await
                .unwrap();
                serve_record_request(&mut peer, scope, BODY_LIMITS, async |key, ids, budget| {
                    assert_eq!(*key, public([11; 32]));
                    assert_eq!(ids, &[id]);
                    assert!(body.len() <= budget);
                    Ok::<_, Infallible>(vec![Record {
                        id,
                        body: body.to_vec(),
                    }])
                })
                .await
                .unwrap();
            }
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn concurrent_peers_resume_from_committed_prefix_after_disconnect() {
    use std::time::Duration;
    use tokio::{
        task::{JoinSet, LocalSet},
        time::sleep,
    };

    const PEERS: u8 = 6;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let scope = [7; 32];
    const EXPECTED: [[u8; 32]; 3] = [[1; 32], [2; 32], [3; 32]];

    tokio::time::timeout(
        Duration::from_secs(10),
        LocalSet::new().run_until(async {
            let mut tasks = JoinSet::new();
            for index in 1..=PEERS {
                tasks.spawn_local(async move {
                    let remote = CompleteInventory::from_ids(&EXPECTED, EXPECTED.len()).unwrap();
                    let mut stored = Vec::new();

                    for attempt in 0..2 {
                        let mut peer = PeerSession::connect(
                            TcpStream::connect(address).await.unwrap(),
                            &[index; 32],
                            &public([22; 32]),
                        )
                        .await
                        .unwrap();
                        let local = CompleteInventory::from_ids(&stored, EXPECTED.len()).unwrap();
                        let outcome = pull_over_session(
                            &mut peer,
                            scope,
                            &remote,
                            &local,
                            BODY_LIMITS,
                            |records| {
                                let record = &records[0];
                                assert_eq!(record.body.as_slice(), &record.id[..1]);
                                stored.push(record.id);
                                Ok::<_, Infallible>(())
                            },
                        )
                        .await;
                        if attempt == 0 {
                            assert_eq!(outcome.unwrap_err().committed, 1);
                            assert_eq!(stored, EXPECTED[..1]);
                        } else {
                            assert_eq!(outcome.unwrap(), 2);
                            assert_eq!(stored, EXPECTED);
                        }
                    }
                });
            }

            let allowed: Vec<_> = (1..=PEERS).map(|index| public([index; 32])).collect();
            let mut attempts = std::collections::HashMap::new();
            for _ in 0..PEERS * 2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut peer = PeerSession::accept(stream, &[22; 32], |key| allowed.contains(key))
                    .await
                    .unwrap();
                let attempt = attempts.entry(*peer.peer_key()).or_insert(0);
                let remaining = if *attempt == 0 {
                    &EXPECTED[..1]
                } else {
                    &EXPECTED[1..]
                };
                *attempt += 1;
                tasks.spawn_local(async move {
                    for id in remaining {
                        serve_record_request(&mut peer, scope, BODY_LIMITS, async |_, ids, _| {
                            assert_eq!(ids, &[*id]);
                            sleep(Duration::from_millis(20 + u64::from(id[0]) * 10)).await;
                            Ok::<_, Infallible>(vec![Record {
                                id: *id,
                                body: vec![id[0]],
                            }])
                        })
                        .await
                        .unwrap();
                    }
                    // The first connection is lost after one acknowledged batch.
                });
            }
            while let Some(result) = tasks.join_next().await {
                result.unwrap();
            }
        }),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn bounded_record_turns_resume_on_the_same_authenticated_session() {
    use mesh_replication::pull::{PullTurnLimits, pull_over_session_bounded};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let bodies = [b"first".to_vec(), b"second".to_vec()];
    let ids: Vec<[u8; 32]> = bodies.iter().map(|b| Sha256::digest(b).into()).collect();
    let remote = CompleteInventory::from_ids(&ids, 2).unwrap();
    let scope = [9; 32];
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(
            async {
                let stream = TcpStream::connect(address).await.unwrap();
                let mut peer = PeerSession::connect(stream, &[11; 32], &public([22; 32]))
                    .await
                    .unwrap();
                let mut stored = Vec::new();
                for turn in 0..2 {
                    let local = CompleteInventory::from_ids(&stored, 2).unwrap();
                    let p = pull_over_session_bounded(
                        &mut peer,
                        scope,
                        &remote,
                        &local,
                        BODY_LIMITS,
                        PullTurnLimits {
                            records: 1,
                            bytes: 64,
                            requests: 1,
                        },
                        |records| {
                            for record in records {
                                assert_eq!(
                                    <[u8; 32]>::from(Sha256::digest(&record.body)),
                                    record.id
                                );
                                stored.push(record.id);
                            }
                            Ok::<_, Infallible>(())
                        },
                    )
                    .await
                    .unwrap();
                    assert_eq!(p.committed, 1);
                    assert_eq!(p.complete, turn == 1);
                }
                assert_eq!(stored.len(), 2);
            },
            async {
                let (stream, _) = listener.accept().await.unwrap();
                let mut peer =
                    PeerSession::accept(stream, &[22; 32], |key| *key == public([11; 32]))
                        .await
                        .unwrap();
                for _ in 0..2 {
                    serve_record_request(&mut peer, scope, BODY_LIMITS, async |_, requested, _| {
                        Ok::<_, Infallible>(
                            requested
                                .iter()
                                .map(|id| {
                                    let index =
                                        ids.iter().position(|candidate| candidate == id).unwrap();
                                    Record {
                                        id: *id,
                                        body: bodies[index].clone(),
                                    }
                                })
                                .collect(),
                        )
                    })
                    .await
                    .unwrap();
                }
            }
        );
    })
    .await
    .unwrap();
}
