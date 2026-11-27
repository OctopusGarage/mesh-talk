//! A small host protocol over pinned TCP sessions, SDK range/pull, and SQLite.
//! This executable owns its protocol, scope grant and transaction. A trusted
//! carrier and a content hash do not prove application author signatures.
use mesh_connect::{PeerSession, TransportError};
use mesh_identity::device::DeviceIdentity;
use mesh_replication::{
    pull::{self, PullLimits, Record},
    range::{self, Checkpoint, Progress, RangeIndex},
    reader::{self, ReaderSource},
    session::ReconciliationSession,
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Duration};
use tokio::net::{TcpListener, TcpStream};
#[path = "support/sqlite_store.rs"]
mod sqlite_store;
use sqlite_store::{AUTHORITY, Result, SCOPE, Store};
const PROFILE: &[u8] = b"MF-example-persistent-v1";
const BATCH: PullLimits = PullLimits {
    records_per_batch: 32,
    bytes_per_batch: 60 * 1024,
};

// The example host selects a subprotocol before each SDK request. This is not a
// new SDK wire profile and must never be sent to an unnegotiated application.
struct BodyPhase<'a>(&'a mut PeerSession<TcpStream>);
impl ReconciliationSession for BodyPhase<'_> {
    type Peer = [u8; 32];
    type Error = TransportError;
    fn peer_identity(&self) -> &Self::Peer {
        self.0.peer_key()
    }
    async fn send(&mut self, frame: &[u8]) -> std::result::Result<(), Self::Error> {
        self.0.send(b"body").await?;
        self.0.send(frame).await
    }
    async fn recv(&mut self) -> std::result::Result<Vec<u8>, Self::Error> {
        self.0.recv().await
    }
}

fn source_binding(index: &RangeIndex) -> Vec<u8> {
    let revision = index.revision();
    let view: [u8; 32] = Sha256::digest(
        [
            index.summary(range::Prefix::root()).digest.as_slice(),
            &revision.to_le_bytes(),
        ]
        .concat(),
    )
    .into();
    let mut binding = SCOPE.to_vec();
    binding.extend(view);
    binding.extend(revision.to_le_bytes());
    binding
}

async fn serve(
    listener: TcpListener,
    secret: &[u8; 32],
    client: [u8; 32],
    index: &RangeIndex,
    records: &BTreeMap<[u8; 32], Record>,
) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let mut peer = PeerSession::accept(stream, secret, |key| *key == client).await?;
    if peer.recv().await? != PROFILE {
        return Err("unknown example profile".into());
    }
    // View and revision describe this immutable, borrowed source for this session.
    let admitted = *peer.peer_key();
    let binding = source_binding(index);
    let view = binding[32..64].try_into()?;
    peer.send(&binding).await?;
    for _ in 0..128 {
        match peer.recv().await?.as_slice() {
            b"summary" => {
                range::serve_summary(&mut peer, index, SCOPE, view, || client == admitted).await?
            }
            b"body" => {
                pull::serve_record_request(&mut peer, SCOPE, BATCH, async |key, ids, budget| {
                    if *key != client {
                        return Err(std::io::Error::other("scope grant denied"));
                    }
                    let mut total = 0usize;
                    let mut result = Vec::new();
                    for id in ids {
                        let record = records
                            .get(id)
                            .ok_or_else(|| std::io::Error::other("missing body"))?;
                        total = total
                            .checked_add(record.body.len())
                            .filter(|n| *n <= budget)
                            .ok_or_else(|| std::io::Error::other("body allowance"))?;
                        result.push(record.clone());
                    }
                    Ok::<_, std::io::Error>(result)
                })
                .await?
            }
            b"done" => return Ok(()),
            _ => return Err("unknown example command".into()),
        }
    }
    Err("example session request allowance exhausted".into())
}

async fn receive(
    address: std::net::SocketAddr,
    secret: &[u8; 32],
    server: [u8; 32],
    store: &mut Store,
    allow_commit: bool,
) -> Result<usize> {
    let stream = TcpStream::connect(address).await?;
    let mut peer = PeerSession::connect(stream, secret, &server).await?;
    peer.send(PROFILE).await?;
    let binding = peer.recv().await?;
    if binding.len() != 72 || binding[..32] != SCOPE {
        return Err("invalid source binding".into());
    }
    let view: [u8; 32] = binding[32..64].try_into()?;
    let revision = u64::from_le_bytes(binding[64..].try_into()?);
    let principal = *peer.peer_key();
    // Match one wire body batch; larger discovery leaves fail explicitly.
    let mut limits = store.limits;
    limits.records = limits.records.min(BATCH.records_per_batch);
    let mut committed = 0;
    // A bounded demonstration, not automatic application history draining.
    for _ in 0..32 {
        let prepared = {
            let snapshot = store.snapshot()?;
            let checkpoint = match store.checkpoint.as_ref() {
                Some(cp)
                    if cp.valid_reader_binding(&snapshot, SCOPE, view, AUTHORITY, revision)? =>
                {
                    cp.clone()
                }
                _ => Checkpoint::for_reader(SCOPE, AUTHORITY, &snapshot, revision)?,
            };
            reader::prepare(
                &checkpoint,
                &snapshot,
                ReaderSource {
                    scope: SCOPE,
                    authority: AUTHORITY,
                    view,
                    revision,
                },
                limits,
                || principal == server,
                async |prefix| {
                    peer.send(b"summary")
                        .await
                        .map_err(range::RemoteError::Exchange)?;
                    range::query_summary(&mut peer, SCOPE, view, prefix).await
                },
            )
            .await?
        };
        // A bounded fetch has no commit callback or durability claim. Only the
        // prepared turn's actual host transaction accepts the verified bodies.
        let staged =
            pull::fetch_records(&mut BodyPhase(&mut peer), SCOPE, prepared.missing(), BATCH)
                .await?;
        let accepted = staged.len();
        // Recheck host scope grant inside the actual transaction. Application
        // author signatures/schema would be validated here too when required.
        let (next, difference) = prepared.commit(|_| {
            store.commit(
                &staged,
                |_tx| Ok(allow_commit && principal == server),
                false,
            )
        })?;
        store.checkpoint = Some(next);
        committed += accepted;
        if difference.progress == Progress::Complete {
            peer.send(b"done").await?;
            return Ok(committed);
        }
        tokio::task::yield_now().await;
    }
    Err("example discovery turn allowance exhausted".into())
}

async fn exchange(
    store: &mut Store,
    records: &[Record],
    allow_commit: bool,
    wrong_pin: bool,
) -> Result<usize> {
    let mut index = RangeIndex::default();
    let mut source = BTreeMap::new();
    for r in records {
        index.insert(r.id);
        source.insert(r.id, r.clone());
    }
    // Test/demo keys are ephemeral; real hosts load protected keys and trusted pins.
    let server_identity = DeviceIdentity::generate();
    let client_identity = DeviceIdentity::generate();
    let server_key = server_identity.public().x25519_pub;
    let client_key = client_identity.public().x25519_pub;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server_bytes = server_identity.secret_bytes().1;
    let client_bytes = client_identity.secret_bytes().1;
    let (served, received) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(
            serve(listener, &server_bytes, client_key, &index, &source),
            receive(
                address,
                &client_bytes,
                if wrong_pin { [0; 32] } else { server_key },
                store,
                allow_commit
            )
        )
    })
    .await?;
    // Preserve the caller-facing failure (e.g. denied commit), even if closing
    // its session makes the server observe EOF. Both halves have now retired.
    let committed = received?;
    served?;
    Ok(committed)
}
fn record(body: &[u8]) -> Record {
    Record {
        id: Sha256::digest(body).into(),
        body: body.to_vec(),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("peer.sqlite");
    let mut records = vec![
        record(b"first opaque record"),
        record(b"second opaque record"),
    ];
    let mut store = Store::open(&path)?;
    let first = exchange(&mut store, &records, true, false).await?;
    assert_eq!(first, 2);
    drop(store);
    let mut store = Store::open(&path)?;
    let duplicate = exchange(&mut store, &records, true, false).await?;
    assert_eq!(duplicate, 0);
    records.push(record(b"later opaque record"));
    let appended = exchange(&mut store, &records, true, false).await?;
    assert_eq!(appended, 1);
    assert_eq!(store.count()?, 3);
    println!(
        "Pinned TCP → SQLite: {first} initial, {duplicate} duplicate, {appended} after append; 3 durable records"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn real_peer_reopen_and_append_are_idempotent() -> Result<()> {
        main_flow().await
    }
    async fn main_flow() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("peer.sqlite");
        let records = vec![record(b"a"), record(b"b")];
        let mut store = Store::open(&path)?;
        assert_eq!(exchange(&mut store, &records, true, false).await?, 2);
        drop(store);
        let mut store = Store::open(&path)?;
        assert_eq!(exchange(&mut store, &records, true, false).await?, 0);
        let mut later = records;
        later.push(record(b"c"));
        assert_eq!(exchange(&mut store, &later, true, false).await?, 1);
        assert_eq!(store.count()?, 3);
        Ok(())
    }
    #[tokio::test]
    async fn multiple_bounded_turns_fetch_only_the_missing_append() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("peer.sqlite");
        let mut records: Vec<_> = (0u32..65).map(|n| record(&n.to_le_bytes())).collect();
        let mut store = Store::open(&path)?;
        assert_eq!(exchange(&mut store, &records, true, false).await?, 65);
        drop(store);
        let mut store = Store::open(&path)?;
        records.push(record(b"only new body after restart"));
        assert_eq!(exchange(&mut store, &records, true, false).await?, 1);
        assert_eq!(store.count()?, 66);
        Ok(())
    }
    #[tokio::test]
    async fn cancelled_body_receive_has_no_commit_and_retries_on_a_fresh_session() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::open(&directory.path().join("peer.sqlite"))?;
        let records = vec![record(b"only accepted after a fresh session")];
        let mut index = RangeIndex::default();
        index.insert(records[0].id);
        let server_identity = DeviceIdentity::generate();
        let client_identity = DeviceIdentity::generate();
        let server_key = server_identity.public().x25519_pub;
        let client_key = client_identity.public().x25519_pub;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let entered_body = tokio::sync::Notify::new();
        let server = async {
            let (stream, _) = listener.accept().await?;
            let mut peer = PeerSession::accept(stream, &server_identity.secret_bytes().1, |key| {
                *key == client_key
            })
            .await?;
            assert_eq!(peer.recv().await?, PROFILE);
            let binding = source_binding(&index);
            let view = binding[32..64].try_into()?;
            peer.send(&binding).await?;
            loop {
                match peer.recv().await?.as_slice() {
                    b"summary" => {
                        range::serve_summary(&mut peer, &index, SCOPE, view, || true).await?
                    }
                    b"body" => {
                        // Receive the actual SDK request, then deliberately hold
                        // its response. The client is cancelled during body recv.
                        let request = peer.recv().await?;
                        assert!(request.starts_with(b"MRQ1\0"));
                        entered_body.notify_one();
                        assert!(
                            peer.recv().await.is_err(),
                            "cancelled client must close this session"
                        );
                        return Ok::<_, Box<dyn std::error::Error>>(());
                    }
                    _ => return Err("unexpected cancellation-test phase".into()),
                }
            }
        };
        let mut server = Box::pin(server);
        {
            let secret = client_identity.secret_bytes().1;
            let mut client = Box::pin(receive(address, &secret, server_key, &mut store, true));
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! {
                    result = &mut server => panic!("source finished before body pause: {result:?}"),
                    result = &mut client => panic!("client finished before body pause: {result:?}"),
                    () = entered_body.notified() => (),
                }
            })
            .await?;
            // Drop owns cancellation; no detached transport/producer remains.
        }
        tokio::time::timeout(Duration::from_secs(5), server).await??;
        assert_eq!(store.count()?, 0);
        assert!(store.checkpoint.is_none());
        assert_eq!(exchange(&mut store, &records, true, false).await?, 1);
        assert_eq!(store.count()?, 1);
        Ok(())
    }
    #[tokio::test]
    async fn wrong_pin_and_revoked_commit_leave_database_and_checkpoint_unchanged() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::open(&directory.path().join("peer.sqlite"))?;
        let records = vec![record(b"not admitted")];
        assert!(exchange(&mut store, &records, true, true).await.is_err());
        assert!(exchange(&mut store, &records, false, false).await.is_err());
        assert_eq!(store.count()?, 0);
        assert!(store.checkpoint.is_none());
        Ok(())
    }
}
