//! Bounded inventory and opaque mailbox reconciliation between pinned relays.
//! The requester and responder both use the ordinary durable sync protocol;
//! neither handles plaintext messages or impersonates another relay.

use crate::eventlog::LogError;
use crate::eventlog::event::{ConversationId, Event, EventId};
use crate::eventlog::store::AppendOutcome;
use crate::eventlog::sync::SyncStore;
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::node::bridge_directory::{
    BridgeDirectory, FederatedPresence, FederatedPresenceSource, FederatedPresenceWithOrigin,
};
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

const REQUEST_V4: &[u8] = b"MTFI4Q";
const PRESENCE_ORIGIN_FRAME: &[u8] = b"MTFP3D";
const REQUEST: &[u8] = b"MTFI1Q";
const REQUEST_V3: &[u8] = b"MTFI3Q";
const REQUEST_V2: &[u8] = b"MTFI2Q";
const RESPONSE: &[u8] = b"MTFI1R";
const IDS_PER_PAGE: usize = 512;
const MAX_PAGES: usize = 256;
const ROUND_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum mailbox work in one bounded exchange; a full batch schedules continuation.
pub const MAX_MAILBOXES_PER_EXCHANGE: usize = 16;
const ROUTED_PRESENCE_FRAME: &[u8] = b"MTFP2D";
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
    fn requires_channel_v2(&self, conversation: &ConversationId) -> bool {
        self.store
            .lock()
            .expect("post office lock not poisoned")
            .requires_channel_v2(conversation)
    }

    fn requires_channel_v3(&self, conversation: &ConversationId) -> bool {
        self.store
            .lock()
            .expect("post office lock not poisoned")
            .requires_channel_v3(conversation)
    }

    fn requires_channel_v4(&self, conversation: &ConversationId) -> bool {
        self.store
            .lock()
            .expect("post office lock not poisoned")
            .requires_channel_v4(conversation)
    }

    fn fingerprint_and_len(&self, conversation: &ConversationId) -> ([u8; 32], usize) {
        self.store
            .lock()
            .expect("post office lock not poisoned")
            .fingerprint_and_len(conversation)
    }

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

    fn events_excluding_bounded(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
        event_budget: usize,
        max_frames: usize,
    ) -> Vec<Event> {
        self.store
            .lock()
            .expect("post office lock not poisoned")
            .events_excluding_bounded(conversation, have, event_budget, max_frames)
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

    pub fn set_office_name(&self, name: &str) {
        self.0
            .lock()
            .expect("directory lock not poisoned")
            .set_office_name(name);
    }

    pub fn subscribe_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.0
            .lock()
            .expect("directory lock not poisoned")
            .subscribe_changes()
    }

    pub fn has_live_presence(&self) -> bool {
        !self
            .0
            .lock()
            .expect("directory lock not poisoned")
            .export_federated(Instant::now(), unix_now())
            .is_empty()
    }

    pub(crate) fn shared(&self) -> Arc<Mutex<BridgeDirectory>> {
        Arc::clone(&self.0)
    }

    /// Network mode requires mutual membership before any inventory/presence.
    /// It never retries the legacy pin-only protocol on authorization failure.
    pub async fn sync_with_network_relay(
        &self,
        addr: SocketAddr,
        identity: &DeviceIdentity,
        expected: &PublicIdentity,
        store: &Arc<Mutex<PostOffice>>,
        network: &super::network::RelayNetwork,
    ) -> Result<usize, String> {
        tokio::time::timeout(ROUND_TIMEOUT, async {
            for request in [REQUEST_V4, REQUEST_V3, REQUEST_V2] {
                match sync_version(
                    addr,
                    identity,
                    expected,
                    store,
                    Some(&self.0),
                    request,
                    Some(network),
                )
                .await
                {
                    Err(error) if error == "unsupported federation version" => continue,
                    result => return result,
                }
            }
            Err("unsupported federation version".to_string())
        })
        .await
        .map_err(|_| "federation exchange deadline exceeded".to_string())?
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

async fn send_frame<IO>(channel: &mut SecureChannel<IO>, bytes: &[u8]) -> Result<(), String>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    tokio::time::timeout(ROUND_TIMEOUT, channel.send(bytes)).await
        .map_err(|_| "federation write deadline exceeded".to_string())?
        .map_err(|e| {
            if matches!(&e, crate::transport::TransportError::Io(io) if matches!(io.kind(),
                std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::UnexpectedEof)) {
                "unsupported federation version".to_string()
            } else { e.to_string() }
        })
}

fn encode_origin_page(
    proofs: &[FederatedPresenceWithOrigin],
    last: bool,
) -> Result<Vec<u8>, String> {
    let mut bytes = PRESENCE_ORIGIN_FRAME.to_vec();
    bytes.push(u8::from(last));
    bytes.extend(bincode::serialize(proofs).map_err(|error| error.to_string())?);
    (bytes.len() <= MAX_PLAINTEXT)
        .then_some(bytes)
        .ok_or_else(|| "federation origin page is too large".into())
}

fn decode_origin_page(bytes: &[u8]) -> Result<(Vec<FederatedPresenceWithOrigin>, bool), String> {
    let body = bytes
        .strip_prefix(PRESENCE_ORIGIN_FRAME)
        .ok_or("invalid federation origin frame")?;
    if bytes.len() > MAX_PLAINTEXT || body.is_empty() || body[0] > 1 {
        return Err("invalid federation origin page".into());
    }
    let mut proofs: Vec<FederatedPresenceWithOrigin> = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_PLAINTEXT as u64)
        .reject_trailing_bytes()
        .deserialize(&body[1..])
        .map_err(|error| error.to_string())?;
    let now = unix_now();
    if proofs.len() > PRESENCE_PER_PAGE
        || proofs.iter().any(|entry| {
            entry
                .origin_hops
                .is_some_and(|h| h > crate::node::bridge_directory::MAX_ORIGIN_HOPS)
        })
    {
        return Err("federation origin count exceeds bound".into());
    }
    for entry in &mut proofs {
        entry.origin = entry
            .origin
            .take()
            .filter(|origin| origin.valid_for(&entry.presence.announce.public(), now));
    }
    Ok((proofs, body[0] == 1))
}

async fn send_presence_pages<IO>(
    channel: &mut SecureChannel<IO>,
    directory: Option<&Arc<Mutex<BridgeDirectory>>>,
    limit: usize,
    routed: bool,
    origins: bool,
) -> Result<(), String>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    if origins {
        let mut proofs = directory
            .map(|directory| {
                directory
                    .lock()
                    .expect("directory lock not poisoned")
                    .export_with_origins(Instant::now(), unix_now())
            })
            .unwrap_or_default();
        proofs.truncate(limit);
        if proofs.is_empty() {
            send_frame(channel, &encode_origin_page(&[], true)?).await?;
        } else {
            let count = proofs.len().div_ceil(PRESENCE_PER_PAGE);
            for (index, page) in proofs.chunks(PRESENCE_PER_PAGE).enumerate() {
                send_frame(channel, &encode_origin_page(page, index + 1 == count)?).await?;
            }
        }
        return Ok(());
    }
    let mut proofs = directory
        .map(|directory| {
            directory
                .lock()
                .expect("directory lock not poisoned")
                .export_presence_sources(Instant::now(), unix_now())
        })
        .unwrap_or_default();
    proofs.truncate(limit);
    let encode = |entries: &[FederatedPresenceSource], last| {
        if routed {
            let mut bytes = Vec::from(ROUTED_PRESENCE_FRAME);
            bytes.push(u8::from(last));
            bytes.extend_from_slice(&bincode::serialize(entries).map_err(|e| e.to_string())?);
            (bytes.len() <= MAX_PLAINTEXT)
                .then_some(bytes)
                .ok_or_else(|| "federation presence page is too large".to_string())
        } else {
            let legacy: Vec<_> = entries.iter().map(|entry| entry.presence.clone()).collect();
            encode_presence_page(&legacy, last)
        }
    };
    let chunks = proofs.chunks(PRESENCE_PER_PAGE).collect::<Vec<_>>();
    if chunks.is_empty() {
        send_frame(channel, &encode(&[], true)?).await?;
    } else {
        for (index, chunk) in chunks.iter().enumerate() {
            send_frame(channel, &encode(chunk, index + 1 == chunks.len())?).await?;
        }
    }
    Ok(())
}

async fn receive_presence_pages<IO>(
    channel: &mut SecureChannel<IO>,
    directory: Option<&Arc<Mutex<BridgeDirectory>>>,
    limit: usize,
    routed: bool,
    origins: bool,
) -> Result<(), String>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    if origins {
        let mut proofs = Vec::new();
        let mut complete = false;
        for _ in 0..limit.div_ceil(PRESENCE_PER_PAGE) {
            let bytes = tokio::time::timeout(ROUND_TIMEOUT, channel.recv())
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?;
            let (page, last) = decode_origin_page(&bytes)?;
            proofs.extend(page);
            if proofs.len() > limit {
                return Err("federation origin count exceeds bound".into());
            }
            if last {
                complete = true;
                break;
            }
        }
        if !complete {
            return Err("incomplete federation origin presence".into());
        }
        if let Some(directory) = directory {
            directory
                .lock()
                .map_err(|error| error.to_string())?
                .import_with_origins(&proofs, unix_now());
        }
        return Ok(());
    }
    let mut proofs = Vec::new();
    let mut complete = false;
    for _ in 0..limit.div_ceil(PRESENCE_PER_PAGE) {
        let bytes = tokio::time::timeout(ROUND_TIMEOUT, channel.recv())
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let (page, last) = if routed {
            let body = bytes
                .strip_prefix(ROUTED_PRESENCE_FRAME)
                .ok_or_else(|| "invalid routed federation presence frame".to_string())?;
            if bytes.len() > MAX_PLAINTEXT || body.is_empty() || body[0] > 1 {
                return Err("invalid routed federation presence page".into());
            }
            let page: Vec<FederatedPresenceSource> = bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .with_limit(MAX_PLAINTEXT as u64)
                .reject_trailing_bytes()
                .deserialize(&body[1..])
                .map_err(|e| e.to_string())?;
            if page.len() > PRESENCE_PER_PAGE
                || page.iter().any(|entry| {
                    entry
                        .origin_hops
                        .is_some_and(|h| h > crate::node::bridge_directory::MAX_ORIGIN_HOPS)
                })
            {
                return Err("invalid federation origin distance".into());
            }
            (page, body[0] == 1)
        } else {
            let (page, last) = decode_presence_page(&bytes)?;
            (
                page.into_iter()
                    .map(|presence| FederatedPresenceSource {
                        presence,
                        origin_hops: None,
                    })
                    .collect(),
                last,
            )
        };
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
        let mut directory = directory.lock().map_err(|e| e.to_string())?;
        if routed {
            directory.import_presence_sources(&proofs, unix_now());
        } else {
            let legacy: Vec<_> = proofs.into_iter().map(|entry| entry.presence).collect();
            directory.import_federated(&legacy, unix_now());
        }
    }
    Ok(())
}

pub(crate) fn is_inventory_request(bytes: &[u8]) -> bool {
    bytes == REQUEST || bytes == REQUEST_V2 || bytes == REQUEST_V3 || bytes == REQUEST_V4
}

pub(crate) fn presence_limit(request: &[u8]) -> usize {
    if request == REQUEST_V2 || request == REQUEST_V3 || request == REQUEST_V4 {
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
        send_frame(channel, &encode_page(&[], true)?).await?;
    } else {
        for (index, chunk) in chunks.iter().enumerate() {
            send_frame(channel, &encode_page(chunk, index + 1 == chunks.len())?).await?;
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
    for page_number in 0..MAX_PAGES {
        let page = tokio::time::timeout(ROUND_TIMEOUT, channel.recv())
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| {
                if page_number == 0
                    && matches!(&e, crate::transport::TransportError::Io(io)
                    if matches!(io.kind(), std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset))
                {
                    "unsupported federation version".to_string()
                } else {
                    e.to_string()
                }
            })?;
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
    request: &[u8],
) -> Result<FederationStore, String>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    tokio::time::timeout(
        ROUND_TIMEOUT,
        send_inventory_inner(channel, store, directory, request),
    )
    .await
    .map_err(|_| "federation inventory deadline exceeded".to_string())?
}

async fn send_inventory_inner<IO>(
    channel: &mut SecureChannel<IO>,
    store: &Arc<Mutex<PostOffice>>,
    directory: Option<&Arc<Mutex<BridgeDirectory>>>,
    request: &[u8],
) -> Result<FederationStore, String>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let limit = presence_limit(request);
    let routed = request == REQUEST_V3 || request == REQUEST_V4;
    let origins = request == REQUEST_V4;
    receive_presence_pages(channel, directory, limit, routed, origins).await?;
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
    send_presence_pages(channel, directory, limit, routed, origins).await?;
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
    tokio::time::timeout(ROUND_TIMEOUT, async {
        for request in [REQUEST_V4, REQUEST_V3, REQUEST_V2, REQUEST] {
            match sync_version(addr, identity, expected, store, directory, request, None).await {
                Ok(count) => return Ok(count),
                // Only a pre-response authenticated close negotiates a lower version.
                Err(error) if error == "unsupported federation version" => continue,
                Err(error) => return Err(error),
            }
        }
        Err("unsupported federation version".to_string())
    })
    .await
    .map_err(|_| "federation exchange deadline exceeded".to_string())?
}

async fn sync_version(
    addr: SocketAddr,
    identity: &DeviceIdentity,
    expected: &PublicIdentity,
    store: &Arc<Mutex<PostOffice>>,
    directory: Option<&Arc<Mutex<BridgeDirectory>>>,
    request: &[u8],
    network: Option<&super::network::RelayNetwork>,
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
    if let Some(network) = network {
        network.connect(&mut channel).await?;
    }
    send_frame(&mut channel, request).await?;
    send_presence_pages(
        &mut channel,
        directory,
        limit,
        request == REQUEST_V3 || request == REQUEST_V4,
        request == REQUEST_V4,
    )
    .await?;
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
    receive_presence_pages(
        &mut channel,
        directory,
        limit,
        request == REQUEST_V3 || request == REQUEST_V4,
        request == REQUEST_V4,
    )
    .await?;
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
    let ids = store.lock().map_err(|e| e.to_string())?.federation_batch(
        expected,
        changed.into_iter().collect(),
        MAX_MAILBOXES_PER_EXCHANGE,
    );
    let session_store = Mutex::new(FederationStore::new(store, remote));
    for id in &ids {
        // Advance before awaiting: a timeout or malformed mailbox cannot pin
        // every later conversation behind the same failing first item.
        store
            .lock()
            .map_err(|e| e.to_string())?
            .advance_federation_cursor(expected, *id);
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
    fn bounded_federation_exports_reuse_the_mailbox_page() {
        use crate::eventlog::EventKind;
        let dir = tempfile::tempdir().unwrap();
        let mut office = PostOffice::open(
            &dir.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        let author = DeviceIdentity::generate();
        let conversation = ConversationId::new([1; 32]);
        let mut expected = Vec::new();
        for seq in 1..=16 {
            let event = Event::new(
                &author,
                conversation,
                seq,
                vec![],
                seq,
                0,
                EventKind::Message,
                vec![7; 32 * 1024],
            );
            expected.push(event.id);
            office.accept(event).unwrap();
        }
        let store = Arc::new(Mutex::new(office));
        let session = FederationStore::new(&store, vec![]);
        let have = HashSet::from([expected.remove(0)]);
        for max_frames in [0, 1, 2, 4] {
            let page =
                session.events_excluding_bounded(&conversation, &have, MAX_PLAINTEXT, max_frames);
            assert_eq!(
                page.iter().map(|event| event.id).collect::<Vec<_>>(),
                expected[..max_frames]
            );
        }
        assert_eq!(
            store
                .lock()
                .unwrap()
                .log
                .sync_metadata_observations(&conversation)
                .1,
            0,
            "federation pages must not export every retained ciphertext first"
        );
    }

    #[tokio::test]
    async fn idle_federation_probes_reuse_metadata_over_loopback() {
        use crate::eventlog::{EventKind, store::EventLog};
        use crate::node::session::{Served, request_round, serve_one};
        use crate::postoffice::opaque::RelayMailbox;
        use tokio::net::TcpStream;

        tokio::time::timeout(Duration::from_secs(5), async {
            let dir = tempfile::tempdir().unwrap();
            let identity = DeviceIdentity::generate();
            let sender = DeviceIdentity::generate();
            let recipient = DeviceIdentity::generate();
            let mailbox = RelayMailbox::federated(&sender, &recipient.public()).unwrap();
            let event = mailbox
                .wrap(&Event::new(
                    &sender,
                    ConversationId::new([1; 32]),
                    1,
                    vec![],
                    1,
                    0,
                    EventKind::Message,
                    vec![7; 32 * 1024],
                ))
                .unwrap();
            let expected = (crate::eventlog::sync::fingerprint(vec![event.id]), 1);
            let (ed, x) = identity.secret_bytes();
            let store = Arc::new(Mutex::new(
                PostOffice::open(
                    &dir.path().join("relay.log"),
                    "pw",
                    DeviceIdentity::from_secret_bytes(ed, x),
                )
                .unwrap(),
            ));
            store.lock().unwrap().accept(event.clone()).unwrap();
            let mut log = EventLog::default();
            log.append(event).unwrap();
            let requester = Mutex::new(log);
            let session = Mutex::new(FederationStore::new(&store, vec![]));
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let public = identity.public();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut channel = SecureChannel::accept(stream, &identity).await.unwrap();
                let mut probes = 0;
                while let Served::Handled(_) = serve_one(&mut channel, &session).await.unwrap() {
                    probes += 1;
                }
                probes
            });
            let stream = TcpStream::connect(address).await.unwrap();
            let mut channel = SecureChannel::connect(stream, &sender, Some(&public))
                .await
                .unwrap();
            for _ in 0..8 {
                let report = request_round(&mut channel, &requester, mailbox.id())
                    .await
                    .unwrap();
                assert_eq!(report.applied, 0);
            }
            drop(channel);
            assert_eq!(
                server.await.unwrap(),
                8,
                "idle sessions send only fingerprint probes"
            );
            assert_eq!(
                store
                    .lock()
                    .unwrap()
                    .log
                    .sync_metadata_observations(&mailbox.id()),
                (Some(expected), 0),
                "federation must reuse mailbox metadata without exporting ciphertext",
            );
        })
        .await
        .expect("idle federation probes must complete");
    }

    #[tokio::test]
    async fn signed_origin_negotiation_preserves_deployed_v3_presence_layout() {
        let office = DeviceIdentity::generate();
        let user = DeviceIdentity::generate();
        let viewer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&user, &Account::generate(), "Duncan", 47474);
        let mut directory = BridgeDirectory::default();
        directory.set_office_name("Windows 10 office");
        directory
            .register(&user.public(), Some(&proof), true, Instant::now())
            .unwrap();
        let request = crate::node::bridge_directory::origin_request(&user, &office.public(), true);
        assert!(directory.apply_origin_request(
            &request,
            &user.public(),
            Some(&office),
            unix_now()
        ));
        let directory = Arc::new(Mutex::new(directory));
        let (client, server) = tokio::io::duplex(MAX_PLAINTEXT * 2);
        let expected = office.public();
        let (client, server) = tokio::join!(
            SecureChannel::connect(client, &viewer, Some(&expected)),
            SecureChannel::accept(server, &office)
        );
        let mut client = client.unwrap();
        let mut server = server.unwrap();
        for request in [REQUEST_V3, REQUEST_V4] {
            send_presence_pages(
                &mut server,
                Some(&directory),
                presence_limit(request),
                true,
                request == REQUEST_V4,
            )
            .await
            .unwrap();
            let bytes = client.recv().await.unwrap();
            if request == REQUEST_V3 {
                let body = bytes.strip_prefix(ROUTED_PRESENCE_FRAME).unwrap();
                let entries: Vec<FederatedPresenceSource> = bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .reject_trailing_bytes()
                    .deserialize(&body[1..])
                    .unwrap();
                assert_eq!(entries[0].presence.announce.user_id, user.user_id());
                assert_eq!(entries[0].origin_hops, Some(0));
            } else {
                let (entries, last) = decode_origin_page(&bytes).unwrap();
                assert!(last);
                assert_eq!(entries[0].origin_hops, Some(0));
                assert_eq!(
                    entries[0].origin.as_ref().unwrap().office_name(),
                    "Windows 10 office"
                );
            }
        }
    }

    #[test]
    fn expired_optional_origin_does_not_discard_federated_presence() {
        let user = DeviceIdentity::generate();
        let office = DeviceIdentity::generate();
        let now = unix_now();
        let consent =
            crate::node::discovery_origin::OriginConsent::new(&user, &office.public(), now - 1);
        let origin = crate::node::discovery_origin::RegistrationOrigin::new(
            &office,
            "Windows 10 office",
            consent,
        )
        .unwrap();
        let proof = Announce::new_with_account(&user, &Account::generate(), "Duncan", 47474);
        let entries = vec![FederatedPresenceWithOrigin {
            presence: FederatedPresence {
                announce: proof,
                expires_at_secs: now + 10,
            },
            origin: Some(origin),
            origin_hops: Some(0),
        }];
        let (decoded, last) =
            decode_origin_page(&encode_origin_page(&entries, true).unwrap()).unwrap();
        assert!(last);
        assert_eq!(decoded.len(), 1);
        assert!(decoded[0].origin.is_none());
    }

    #[tokio::test]
    async fn federation_inventory_write_releases_a_backpressured_peer() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let (client, server) = tokio::io::duplex(64);
        let expected = b.public();
        let (outgoing, incoming) = tokio::join!(
            SecureChannel::connect(client, &a, Some(&expected)),
            SecureChannel::accept(server, &b),
        );
        let mut outgoing = outgoing.unwrap();
        let _incoming = incoming.unwrap(); // authenticated peer stops reading
        let page = FederatedMailbox {
            id: ConversationId::new([3; 32]),
            digest: [1; 32],
            first_seen_secs: unix_now(),
        };
        tokio::time::pause();
        let task = tokio::spawn(async move { send_inventory_pages(&mut outgoing, &[page]).await });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(31)).await;
        let result = tokio::time::timeout(Duration::from_secs(1), task).await;
        assert!(
            result.is_ok(),
            "authenticated backpressure must have a write deadline"
        );
        assert!(result.unwrap().unwrap().is_err());
    }

    #[tokio::test]
    async fn failed_noise_handshake_does_not_repeat_as_legacy_federation() {
        let temp = tempfile::tempdir().unwrap();
        let local = Arc::new(Mutex::new(
            PostOffice::open(&temp.path().join("relay"), "pw", DeviceIdentity::generate()).unwrap(),
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = count.clone();
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                drop(stream);
            }
        });
        assert!(
            sync_with_relay(
                addr,
                &DeviceIdentity::generate(),
                &DeviceIdentity::generate().public(),
                &local
            )
            .await
            .is_err()
        );
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn complete_federation_exchange_has_one_deadline() {
        let temp = tempfile::tempdir().unwrap();
        let local = Arc::new(Mutex::new(
            PostOffice::open(&temp.path().join("relay"), "pw", DeviceIdentity::generate()).unwrap(),
        ));
        let identity = DeviceIdentity::generate();
        let remote = DeviceIdentity::generate();
        let expected = remote.public();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (accepted, ready) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut channel = SecureChannel::accept(stream, &remote).await.unwrap();
            assert_eq!(channel.recv().await.unwrap(), REQUEST_V4);
            accepted.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let task =
            tokio::spawn(async move { sync_with_relay(addr, &identity, &expected, &local).await });
        ready.await.unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(31)).await;
        let result = tokio::time::timeout(Duration::from_secs(1), task).await;
        server.abort();
        assert!(
            result.is_ok(),
            "transport and compatibility phases must share one deadline"
        );
        assert!(result.unwrap().unwrap().is_err());
    }

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
            let mut newest = SecureChannel::accept(stream, &identity).await.unwrap();
            assert_eq!(newest.recv().await.unwrap(), REQUEST_V4);
            drop(newest);
            let (stream, _) = listener.accept().await.unwrap();
            let mut first = SecureChannel::accept(stream, &identity).await.unwrap();
            assert_eq!(first.recv().await.unwrap(), REQUEST_V3);
            drop(first);
            let (stream, _) = listener.accept().await.unwrap();
            let mut second = SecureChannel::accept(stream, &identity).await.unwrap();
            assert_eq!(second.recv().await.unwrap(), REQUEST_V2);
            drop(second); // Simulate a pre-v2 endpoint rejecting the new request.
            let (stream, _) = listener.accept().await.unwrap();
            let mut legacy = SecureChannel::accept(stream, &identity).await.unwrap();
            assert_eq!(legacy.recv().await.unwrap(), REQUEST);
            send_inventory(&mut legacy, &remote, Some(&target.0), REQUEST)
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
