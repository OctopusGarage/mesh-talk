//! Bounded inventory and opaque mailbox reconciliation between pinned relays.
//! The requester and responder both use the ordinary durable sync protocol;
//! neither handles plaintext messages or impersonates another relay.

use crate::eventlog::LogError;
use crate::eventlog::event::{ConversationId, Event, EventId};
use crate::eventlog::store::AppendOutcome;
use crate::eventlog::sync::SyncStore;
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::node::bridge_directory::{BridgeDirectory, FederatedPresence};
use crate::node::session::request_round;
use crate::postoffice::gateway::GatewayProof;
use crate::postoffice::{FederatedMailbox, MAX_RELAY_AGE, PostOffice};
use crate::transport::{MAX_PLAINTEXT, SecureChannel};
use bincode::Options;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

const REQUEST: &[u8] = b"MTFI1Q";
const REQUEST_V2: &[u8] = b"MTFI2Q";
const RESPONSE: &[u8] = b"MTFI1R";
const IDS_PER_PAGE: usize = 512;
const MAX_PAGES: usize = 256;
const ROUND_TIMEOUT: Duration = Duration::from_secs(30);
const PRESENCE_FRAME: &[u8] = b"MTFP1D";
const PRESENCE_PER_PAGE: usize = 16;

/// Arrival hints belong to one bounded inventory exchange. Unknown mailboxes
/// stay in memory until a valid event establishes durable custody.
pub(crate) struct FederationStore {
    store: Arc<Mutex<PostOffice>>,
    arrivals: HashMap<ConversationId, u64>,
}

impl FederationStore {
    fn new(store: &Arc<Mutex<PostOffice>>, inventory: Vec<FederatedMailbox>) -> Self {
        let mut arrivals = HashMap::with_capacity(inventory.len());
        for entry in inventory {
            arrivals
                .entry(entry.id)
                .and_modify(|seen: &mut u64| *seen = (*seen).min(entry.first_seen_secs))
                .or_insert(entry.first_seen_secs);
        }
        Self {
            store: Arc::clone(store),
            arrivals,
        }
    }
}

impl SyncStore for FederationStore {
    fn durable_have(&self, conversation: &ConversationId, id: &EventId) -> bool {
        self.store
            .lock()
            .expect("post office lock not poisoned")
            .durable_have(conversation, id)
    }

    fn event_ids(&self, conversation: &ConversationId) -> Vec<EventId> {
        self.store
            .lock()
            .expect("post office lock not poisoned")
            .event_ids(conversation)
    }

    fn events_excluding(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
    ) -> Vec<Event> {
        self.store
            .lock()
            .expect("post office lock not poisoned")
            .events_excluding(conversation, have)
    }

    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        let mut office = self.store.lock().expect("post office lock not poisoned");
        if office.opaque_only && !crate::postoffice::opaque::envelope_shape(&event) {
            return Err(LogError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "post office requires an opaque envelope",
            )));
        }
        let origin = self.arrivals.get(&event.conversation_id).copied();
        let now = SystemTime::now();
        if let Some(first_seen_secs) = origin
            && office
                .first_seen
                .get(&event.conversation_id)
                .is_some_and(|&prior| first_seen_secs < prior)
        {
            // Another session may have established custody after this inventory
            // arrived. Correct that existing clock even for a duplicate event.
            office.adopt_federated_arrivals(
                &[FederatedMailbox {
                    id: event.conversation_id,
                    digest: [0; 32],
                    first_seen_secs,
                }],
                now,
            )?;
        }
        office.record_at_with_arrival(event, now, origin)
    }
}

#[derive(Clone, Default)]
pub struct FederationDirectory(Arc<Mutex<BridgeDirectory>>);

impl FederationDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_gateway(gateway: Arc<Mutex<Option<GatewayProof>>>) -> Self {
        Self(Arc::new(Mutex::new(BridgeDirectory::with_gateway(gateway))))
    }

    pub(crate) fn shared(&self) -> Arc<Mutex<BridgeDirectory>> {
        Arc::clone(&self.0)
    }

    pub async fn sync_with_relay(
        &self,
        addr: SocketAddr,
        identity: &DeviceIdentity,
        expected: &PublicIdentity,
        store: &Arc<Mutex<PostOffice>>,
    ) -> Result<usize, String> {
        sync_with_directory(addr, identity, expected, store, Some(&self.0)).await
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn encode_presence_page(proofs: &[FederatedPresence], last: bool) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::from(PRESENCE_FRAME);
    bytes.push(u8::from(last));
    bytes.extend_from_slice(&bincode::serialize(proofs).map_err(|e| e.to_string())?);
    (bytes.len() <= MAX_PLAINTEXT)
        .then_some(bytes)
        .ok_or_else(|| "federation presence page is too large".into())
}

fn decode_presence_page(bytes: &[u8]) -> Result<(Vec<FederatedPresence>, bool), String> {
    let body = bytes
        .strip_prefix(PRESENCE_FRAME)
        .ok_or_else(|| "invalid federation presence frame".to_string())?;
    if bytes.len() > MAX_PLAINTEXT || body.is_empty() || body[0] > 1 {
        return Err("invalid federation presence page".into());
    }
    let proofs: Vec<FederatedPresence> = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_PLAINTEXT as u64)
        .reject_trailing_bytes()
        .deserialize(&body[1..])
        .map_err(|e| e.to_string())?;
    if proofs.len() > PRESENCE_PER_PAGE {
        return Err("federation presence page exceeds bound".into());
    }
    Ok((proofs, body[0] == 1))
}

async fn send_presence_pages<IO>(
    channel: &mut SecureChannel<IO>,
    directory: Option<&Arc<Mutex<BridgeDirectory>>>,
    limit: usize,
) -> Result<(), String>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let mut proofs = directory
        .map(|directory| {
            directory
                .lock()
                .expect("directory lock not poisoned")
                .export_federated(Instant::now(), unix_now())
        })
        .unwrap_or_default();
    proofs.truncate(limit);
    let chunks = proofs.chunks(PRESENCE_PER_PAGE).collect::<Vec<_>>();
    if chunks.is_empty() {
        channel
            .send(&encode_presence_page(&[], true)?)
            .await
            .map_err(|e| e.to_string())?;
    } else {
        for (index, chunk) in chunks.iter().enumerate() {
            channel
                .send(&encode_presence_page(chunk, index + 1 == chunks.len())?)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

async fn receive_presence_pages<IO>(
    channel: &mut SecureChannel<IO>,
    directory: Option<&Arc<Mutex<BridgeDirectory>>>,
    limit: usize,
) -> Result<(), String>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let mut proofs = Vec::new();
    let mut complete = false;
    for _ in 0..limit.div_ceil(PRESENCE_PER_PAGE) {
        let bytes = tokio::time::timeout(ROUND_TIMEOUT, channel.recv())
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let (page, last) = decode_presence_page(&bytes)?;
        proofs.extend(page);
        if last {
            complete = true;
            break;
        }
    }
    if !complete {
        return Err("incomplete federation presence".into());
    }
    if let Some(directory) = directory {
        directory
            .lock()
            .map_err(|e| e.to_string())?
            .import_federated(&proofs, unix_now());
    }
    Ok(())
}

pub(crate) fn is_inventory_request(bytes: &[u8]) -> bool {
    bytes == REQUEST || bytes == REQUEST_V2
}

pub(crate) fn presence_limit(request: &[u8]) -> usize {
    if request == REQUEST_V2 {
        crate::node::bridge_directory::MAX_MEMBERS
    } else {
        crate::node::bridge_directory::LEGACY_MAX_MEMBERS
    }
}

fn encode_page(ids: &[FederatedMailbox], last: bool) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::from(RESPONSE);
    bytes.push(u8::from(last));
    bytes.extend_from_slice(&bincode::serialize(ids).map_err(|e| e.to_string())?);
    (bytes.len() <= MAX_PLAINTEXT)
        .then_some(bytes)
        .ok_or_else(|| "federation inventory page is too large".into())
}

fn decode_page(bytes: &[u8]) -> Result<(Vec<FederatedMailbox>, bool), String> {
    let body = bytes
        .strip_prefix(RESPONSE)
        .ok_or_else(|| "invalid federation inventory frame".to_string())?;
    if bytes.len() > MAX_PLAINTEXT || body.is_empty() || body[0] > 1 {
        return Err("invalid federation inventory page".into());
    }
    let ids: Vec<FederatedMailbox> = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_PLAINTEXT as u64)
        .reject_trailing_bytes()
        .deserialize(&body[1..])
        .map_err(|e| e.to_string())?;
    let now = unix_now();
    if ids.len() > IDS_PER_PAGE
        || ids.iter().any(|entry| {
            !crate::postoffice::opaque::RelayMailbox::is_federated_id(&entry.id)
                || entry.first_seen_secs <= now.saturating_sub(MAX_RELAY_AGE.as_secs())
                || entry.first_seen_secs > now.saturating_add(300)
        })
    {
        return Err("invalid federation mailbox id".into());
    }
    Ok((ids, body[0] == 1))
}

async fn send_inventory_pages<IO>(
    channel: &mut SecureChannel<IO>,
    ids: &[FederatedMailbox],
) -> Result<(), String>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    if ids.len() > IDS_PER_PAGE * MAX_PAGES {
        return Err("federation inventory exceeds configured bound".into());
    }
    let chunks = ids.chunks(IDS_PER_PAGE).collect::<Vec<_>>();
    if chunks.is_empty() {
        channel
            .send(&encode_page(&[], true)?)
            .await
            .map_err(|e| e.to_string())?;
    } else {
        for (index, chunk) in chunks.iter().enumerate() {
            channel
                .send(&encode_page(chunk, index + 1 == chunks.len())?)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

async fn receive_inventory_pages<IO>(
    channel: &mut SecureChannel<IO>,
) -> Result<Vec<FederatedMailbox>, String>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let mut entries = Vec::new();
    let mut complete = false;
    for _ in 0..MAX_PAGES {
        let page = tokio::time::timeout(ROUND_TIMEOUT, channel.recv())
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let (remote, last) = decode_page(&page)?;
        entries.extend(remote);
        if last {
            complete = true;
            break;
        }
    }
    if !complete || entries.len() > IDS_PER_PAGE * MAX_PAGES {
        return Err("incomplete or oversized federation inventory".into());
    }
    Ok(entries)
}

pub(crate) async fn send_inventory<IO>(
    channel: &mut SecureChannel<IO>,
    store: &Arc<Mutex<PostOffice>>,
    directory: Option<&Arc<Mutex<BridgeDirectory>>>,
    limit: usize,
) -> Result<FederationStore, String>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    receive_presence_pages(channel, directory, limit).await?;
    let remote = receive_inventory_pages(channel).await?;
    let mut local = {
        let mut office = store.lock().map_err(|e| e.to_string())?;
        office.prune_expired().map_err(|e| e.to_string())?;
        office
            .adopt_federated_arrivals(&remote, SystemTime::now())
            .map_err(|e| e.to_string())?;
        office.federated_inventory()
    };
    local.sort_by_key(|entry| *entry.id.as_bytes());
    send_inventory_pages(channel, &local).await?;
    send_presence_pages(channel, directory, limit).await?;
    Ok(FederationStore::new(store, remote))
}

/// Reconcile every federated mailbox held by either relay. The remote public
/// identity is pinned at the Noise handshake; a failed path leaves local data
/// durable for the next retry.
pub async fn sync_with_relay(
    addr: SocketAddr,
    identity: &DeviceIdentity,
    expected: &PublicIdentity,
    store: &Arc<Mutex<PostOffice>>,
) -> Result<usize, String> {
    sync_with_directory(addr, identity, expected, store, None).await
}

async fn sync_with_directory(
    addr: SocketAddr,
    identity: &DeviceIdentity,
    expected: &PublicIdentity,
    store: &Arc<Mutex<PostOffice>>,
    directory: Option<&Arc<Mutex<BridgeDirectory>>>,
) -> Result<usize, String> {
    match sync_version(addr, identity, expected, store, directory, REQUEST_V2).await {
        Ok(count) => Ok(count),
        Err(_) => sync_version(addr, identity, expected, store, directory, REQUEST).await,
    }
}

async fn sync_version(
    addr: SocketAddr,
    identity: &DeviceIdentity,
    expected: &PublicIdentity,
    store: &Arc<Mutex<PostOffice>>,
    directory: Option<&Arc<Mutex<BridgeDirectory>>>,
    request: &[u8],
) -> Result<usize, String> {
    let limit = presence_limit(request);
    let stream = tokio::time::timeout(ROUND_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    stream.set_nodelay(true).map_err(|e| e.to_string())?;
    let mut channel = tokio::time::timeout(
        ROUND_TIMEOUT,
        SecureChannel::connect(stream, identity, Some(expected)),
    )
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    channel.send(request).await.map_err(|e| e.to_string())?;
    send_presence_pages(&mut channel, directory, limit).await?;
    let mut local = {
        let mut office = store.lock().map_err(|e| e.to_string())?;
        office.prune_expired().map_err(|e| e.to_string())?;
        office.federated_inventory()
    };
    local.sort_by_key(|entry| *entry.id.as_bytes());
    send_inventory_pages(&mut channel, &local).await?;
    let remote = receive_inventory_pages(&mut channel).await?;
    store
        .lock()
        .map_err(|e| e.to_string())?
        .adopt_federated_arrivals(&remote, SystemTime::now())
        .map_err(|e| e.to_string())?;
    receive_presence_pages(&mut channel, directory, limit).await?;
    let local_digests = local
        .iter()
        .map(|entry| (entry.id, entry.digest))
        .collect::<std::collections::HashMap<_, _>>();
    let remote_ids = remote.iter().map(|entry| entry.id).collect::<HashSet<_>>();
    let mut changed = HashSet::new();
    for entry in &remote {
        if local_digests.get(&entry.id) != Some(&entry.digest) {
            changed.insert(entry.id);
        }
    }
    changed.extend(
        local
            .iter()
            .filter(|entry| !remote_ids.contains(&entry.id))
            .map(|entry| entry.id),
    );
    let mut ids = changed.into_iter().collect::<Vec<_>>();
    ids.sort_by_key(|id| *id.as_bytes());
    let session_store = Mutex::new(FederationStore::new(store, remote));
    for id in &ids {
        tokio::time::timeout(
            ROUND_TIMEOUT,
            request_round(&mut channel, &session_store, *id),
        )
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:?}"))?;
    }
    Ok(ids.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{discovery::Announce, identity::account::Account};
    use tokio::net::TcpListener;

    #[test]
    fn dropping_sessions_with_unknown_inventory_never_persists_hints() {
        use crate::postoffice::opaque::RelayMailbox;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("relay.log");
        let store = Arc::new(Mutex::new(
            PostOffice::open(&path, "pw", DeviceIdentity::generate()).unwrap(),
        ));
        let before = std::fs::read(path.with_extension("retention")).unwrap();
        let sender = DeviceIdentity::generate();
        for _ in 0..16 {
            let recipient = DeviceIdentity::generate();
            let mailbox = RelayMailbox::federated(&sender, &recipient.public()).unwrap();
            let session = FederationStore::new(
                &store,
                vec![FederatedMailbox {
                    id: mailbox.id(),
                    digest: [0; 32],
                    first_seen_secs: unix_now() - 60,
                }],
            );
            assert_eq!(session.arrivals.len(), 1);
            drop(session);
        }
        assert!(store.lock().unwrap().first_seen.is_empty());
        assert_eq!(
            std::fs::read(path.with_extension("retention")).unwrap(),
            before
        );
    }

    #[test]
    fn session_arrivals_are_committed_only_with_valid_events_and_survive_restart() {
        use crate::eventlog::event::EventKind;
        use crate::postoffice::opaque::RelayMailbox;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("relay.log");
        let identity = DeviceIdentity::generate();
        let (ed, x) = identity.secret_bytes();
        let store = Arc::new(Mutex::new(PostOffice::open(&path, "pw", identity).unwrap()));
        store.lock().unwrap().require_opaque_envelopes();
        let before = std::fs::read(path.with_extension("retention")).unwrap();
        let sender = DeviceIdentity::generate();
        let recipient = DeviceIdentity::generate();
        let mailbox = RelayMailbox::federated(&sender, &recipient.public()).unwrap();
        let inner = Event::new(
            &sender,
            ConversationId::new([1; 32]),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"queued".to_vec(),
        );
        let event = mailbox.wrap(&inner).unwrap();
        let origin = unix_now() - 10 * 24 * 60 * 60;
        let inventory = vec![FederatedMailbox {
            id: mailbox.id(),
            digest: [0; 32],
            first_seen_secs: origin,
        }];
        store
            .lock()
            .unwrap()
            .adopt_federated_arrivals(&inventory, SystemTime::now())
            .unwrap();
        let mut session = FederationStore::new(&store, inventory);
        assert_eq!(
            std::fs::read(path.with_extension("retention")).unwrap(),
            before
        );
        let mut forged = event.clone();
        forged.sig[0] ^= 1;
        for _ in 0..8 {
            assert!(matches!(
                session.ingest(forged.clone()),
                Err(LogError::BadSignature)
            ));
        }
        assert_eq!(
            std::fs::read(path.with_extension("retention")).unwrap(),
            before
        );
        assert!(store.lock().unwrap().first_seen.is_empty());
        assert_eq!(
            session.ingest(event.clone()).unwrap(),
            AppendOutcome::Appended
        );
        let accepted = std::fs::read(path.with_extension("retention")).unwrap();
        assert_eq!(
            session.ingest(event.clone()).unwrap(),
            AppendOutcome::Duplicate
        );
        assert_eq!(
            std::fs::read(path.with_extension("retention")).unwrap(),
            accepted
        );
        drop(session);
        drop(store);
        let reopened =
            PostOffice::open(&path, "pw", DeviceIdentity::from_secret_bytes(ed, x)).unwrap();
        assert!(reopened.has(&event.id));
        assert_eq!(reopened.first_seen.get(&mailbox.id()), Some(&origin));
        let mut reopened = reopened;
        reopened
            .prune_expired_at(UNIX_EPOCH + Duration::from_secs(origin) + MAX_RELAY_AGE)
            .unwrap();
        assert!(!reopened.has(&event.id));
    }

    #[test]
    fn session_origin_survives_concurrent_first_custody_and_duplicate_ingest() {
        use crate::eventlog::event::EventKind;
        use crate::postoffice::opaque::RelayMailbox;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("relay.log");
        let identity = DeviceIdentity::generate();
        let (ed, x) = identity.secret_bytes();
        let store = Arc::new(Mutex::new(PostOffice::open(&path, "pw", identity).unwrap()));
        let sender = DeviceIdentity::generate();
        let recipient = DeviceIdentity::generate();
        let mailbox = RelayMailbox::federated(&sender, &recipient.public()).unwrap();
        let inner = Event::new(
            &sender,
            ConversationId::new([1; 32]),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"concurrent custody".to_vec(),
        );
        let event = mailbox.wrap(&inner).unwrap();
        let origin = unix_now() - 10 * 24 * 60 * 60;
        let mut session = FederationStore::new(
            &store,
            vec![FederatedMailbox {
                id: mailbox.id(),
                digest: [0; 32],
                first_seen_secs: origin,
            }],
        );
        assert_eq!(
            store.lock().unwrap().ingest(event.clone()).unwrap(),
            AppendOutcome::Appended
        );
        assert!(store.lock().unwrap().first_seen[&mailbox.id()] > origin);
        assert_eq!(
            session.ingest(event.clone()).unwrap(),
            AppendOutcome::Duplicate
        );
        assert_eq!(store.lock().unwrap().first_seen[&mailbox.id()], origin);
        let accepted = std::fs::read(path.with_extension("retention")).unwrap();
        assert_eq!(
            session.ingest(event.clone()).unwrap(),
            AppendOutcome::Duplicate
        );
        assert_eq!(
            std::fs::read(path.with_extension("retention")).unwrap(),
            accepted
        );
        drop(session);
        drop(store);
        let mut reopened =
            PostOffice::open(&path, "pw", DeviceIdentity::from_secret_bytes(ed, x)).unwrap();
        assert_eq!(reopened.first_seen[&mailbox.id()], origin);
        reopened
            .prune_expired_at(UNIX_EPOCH + Duration::from_secs(origin) + MAX_RELAY_AGE)
            .unwrap();
        assert!(!reopened.has(&event.id));
    }

    #[test]
    fn session_rejects_an_origin_that_expired_after_inventory() {
        use crate::eventlog::event::EventKind;
        use crate::postoffice::opaque::RelayMailbox;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("relay.log");
        let store = Arc::new(Mutex::new(
            PostOffice::open(&path, "pw", DeviceIdentity::generate()).unwrap(),
        ));
        let before = std::fs::read(path.with_extension("retention")).unwrap();
        let sender = DeviceIdentity::generate();
        let recipient = DeviceIdentity::generate();
        let mailbox = RelayMailbox::federated(&sender, &recipient.public()).unwrap();
        let inner = Event::new(
            &sender,
            ConversationId::new([1; 32]),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"expired".to_vec(),
        );
        let event = mailbox.wrap(&inner).unwrap();
        let mut session = FederationStore::new(
            &store,
            vec![FederatedMailbox {
                id: mailbox.id(),
                digest: [0; 32],
                first_seen_secs: unix_now() - MAX_RELAY_AGE.as_secs(),
            }],
        );
        assert!(session.ingest(event.clone()).is_err());
        assert!(!store.lock().unwrap().has(&event.id));
        assert_eq!(
            std::fs::read(path.with_extension("retention")).unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn legacy_peer_fallback_preserves_the_original_presence_bound() {
        let temp = tempfile::tempdir().unwrap();
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let expected = b.public();
        let (ae, ax) = a.secret_bytes();
        let (be, bx) = b.secret_bytes();
        let local = Arc::new(Mutex::new(
            PostOffice::open(
                &temp.path().join("a"),
                "pw",
                DeviceIdentity::from_secret_bytes(ae, ax),
            )
            .unwrap(),
        ));
        let remote = Arc::new(Mutex::new(
            PostOffice::open(&temp.path().join("b"), "pw", b).unwrap(),
        ));
        let source = FederationDirectory::new();
        let target = FederationDirectory::new();
        let account = Account::generate();
        for _ in 0..130 {
            let member = DeviceIdentity::generate();
            let proof = Announce::new_with_account(&member, &account, "member", 47475);
            source
                .0
                .lock()
                .unwrap()
                .register(&member.public(), Some(&proof), true, Instant::now())
                .unwrap();
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let learned = target.clone();
        let server = tokio::spawn(async move {
            let identity = DeviceIdentity::from_secret_bytes(be, bx);
            let (stream, _) = listener.accept().await.unwrap();
            let mut first = SecureChannel::accept(stream, &identity).await.unwrap();
            assert_eq!(first.recv().await.unwrap(), REQUEST_V2);
            drop(first); // Simulate a pre-v2 endpoint rejecting the new request.
            let (stream, _) = listener.accept().await.unwrap();
            let mut legacy = SecureChannel::accept(stream, &identity).await.unwrap();
            assert_eq!(legacy.recv().await.unwrap(), REQUEST);
            send_inventory(
                &mut legacy,
                &remote,
                Some(&target.0),
                presence_limit(REQUEST),
            )
            .await
            .unwrap();
        });
        source
            .sync_with_relay(address, &a, &expected, &local)
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(
            learned
                .0
                .lock()
                .unwrap()
                .export_federated(Instant::now(), unix_now())
                .len(),
            crate::node::bridge_directory::LEGACY_MAX_MEMBERS
        );
    }

    #[tokio::test]
    async fn modern_federation_carries_presence_beyond_legacy_128() {
        let temp = tempfile::tempdir().unwrap();
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let (be, bx) = b.secret_bytes();
        let expected = b.public();
        let local = Arc::new(Mutex::new(
            PostOffice::open(
                &temp.path().join("a"),
                "pw",
                DeviceIdentity::from_secret_bytes(a.secret_bytes().0, a.secret_bytes().1),
            )
            .unwrap(),
        ));
        let remote = Arc::new(Mutex::new(
            PostOffice::open(&temp.path().join("b"), "pw", b).unwrap(),
        ));
        let source = FederationDirectory::new();
        let target = FederationDirectory::new();
        let account = Account::generate();
        for _ in 0..130 {
            let member = DeviceIdentity::generate();
            let proof = Announce::new_with_account(&member, &account, "member", 47475);
            source
                .0
                .lock()
                .unwrap()
                .register(&member.public(), Some(&proof), true, Instant::now())
                .unwrap();
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(
            crate::node::postbox::run_federated_relay_accept_loop_with_directory(
                DeviceIdentity::from_secret_bytes(be, bx),
                listener,
                remote,
                Arc::new(HashSet::from([a.user_id()])),
                target.clone(),
            ),
        );
        let result = source.sync_with_relay(address, &a, &expected, &local).await;
        server.abort();
        result.expect("modern peers must negotiate a bounded presence snapshot above 128");
        assert_eq!(
            target
                .0
                .lock()
                .unwrap()
                .export_federated(Instant::now(), unix_now())
                .len(),
            130
        );
    }
}
