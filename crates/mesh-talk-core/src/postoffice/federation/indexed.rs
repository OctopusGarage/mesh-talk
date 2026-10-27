//! Bounded SQL federation over the existing authenticated relay protocol.
use super::*;
use crate::{
    node::session::{
        RemoteHaveBudget, Served, request_round_budgeted, serve_one_budgeted,
        serve_wire_bytes_budgeted,
    },
    postoffice::{
        IndexedPostOffice,
        indexed::IndexedFederationStore,
        storage::{
            federation::{Direction, SessionLease, SessionOwner},
            role::inventory::view::InventoryView,
        },
    },
};

/// Typed local failures survive negotiation and reach the runtime supervisor.
#[derive(Debug)]
pub enum IndexedRelayError {
    Store(LogError),
    Session(crate::node::session::SessionError),
    Transport(crate::transport::TransportError),
    PreparationPending,
    UnsupportedVersion,
    Protocol(String),
}
impl IndexedRelayError {
    /// Local availability/resource retries never negotiate an older protocol.
    pub fn is_local_retry(&self) -> bool {
        use crate::node::session::SessionError;
        matches!(
            self,
            Self::PreparationPending
                | Self::Store(
                    LogError::StaleCursor
                        | LogError::RelayCapacityUnavailable
                        | LogError::ConversationRetiring
                        | LogError::RelayMailboxUnavailable
                )
                | Self::Session(
                    SessionError::ResourceUnavailable
                        | SessionError::Store(
                            LogError::StaleCursor
                                | LogError::RelayCapacityUnavailable
                                | LogError::ConversationRetiring
                                | LogError::RelayMailboxUnavailable
                        )
                )
        )
    }
}
impl std::fmt::Display for IndexedRelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(e) => e.fmt(f),
            Self::Session(e) => e.fmt(f),
            Self::Transport(e) => e.fmt(f),
            Self::PreparationPending => f.write_str("federation inventory preparation pending"),
            Self::UnsupportedVersion => f.write_str("unsupported federation version"),
            Self::Protocol(e) => f.write_str(e),
        }
    }
}
impl std::error::Error for IndexedRelayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(e) => Some(e),
            Self::Session(e) => Some(e),
            Self::Transport(e) => Some(e),
            _ => None,
        }
    }
}
impl From<LogError> for IndexedRelayError {
    fn from(e: LogError) -> Self {
        Self::Store(e)
    }
}
impl From<crate::node::session::SessionError> for IndexedRelayError {
    fn from(e: crate::node::session::SessionError) -> Self {
        Self::Session(e)
    }
}
impl From<crate::transport::TransportError> for IndexedRelayError {
    fn from(e: crate::transport::TransportError) -> Self {
        Self::Transport(e)
    }
}
impl From<std::io::Error> for IndexedRelayError {
    fn from(e: std::io::Error) -> Self {
        Self::Transport(crate::transport::TransportError::Io(e))
    }
}
// The legacy wire helpers return strings. This conversion is used only for their
// protocol/transport outcomes, never for a local store or session failure.
impl From<String> for IndexedRelayError {
    fn from(e: String) -> Self {
        if e == "unsupported federation version" {
            Self::UnsupportedVersion
        } else {
            Self::Protocol(e)
        }
    }
}
impl From<&str> for IndexedRelayError {
    fn from(e: &str) -> Self {
        e.to_string().into()
    }
}

#[derive(Clone)]
pub enum IndexedRelayTrust {
    Pinned(Arc<HashSet<String>>),
    Network(super::super::network::RelayNetwork),
}
/// One shared endpoint per dedicated runtime. Clones retain the same profile,
/// directory and process-wide Have gate; they never open another connection.
#[derive(Clone)]
pub struct IndexedRelayEndpoint {
    office: IndexedPostOffice,
    directory: Option<FederationDirectory>,
    trust: Option<IndexedRelayTrust>,
    budget: RemoteHaveBudget,
}
impl IndexedRelayEndpoint {
    pub fn new(
        office: IndexedPostOffice,
        directory: Option<FederationDirectory>,
        trust: Option<IndexedRelayTrust>,
    ) -> Self {
        Self {
            office,
            directory,
            trust,
            budget: RemoteHaveBudget::default(),
        }
    }
    /// Compatibility edge for existing SDK callers. Runtime uses checked methods.
    pub async fn sync_with_relay(
        &self,
        addr: SocketAddr,
        identity: &DeviceIdentity,
        expected: &PublicIdentity,
    ) -> Result<usize, String> {
        self.sync_with_relay_checked(addr, identity, expected)
            .await
            .map_err(|e| e.to_string())
    }
    pub async fn serve_connection<IO: AsyncRead + AsyncWrite + Unpin>(
        &self,
        channel: SecureChannel<IO>,
        origin_signer: Option<&DeviceIdentity>,
    ) -> Result<(), String> {
        self.serve_connection_checked(channel, origin_signer)
            .await
            .map_err(|e| e.to_string())
    }
    fn directory(&self) -> Option<&Arc<Mutex<BridgeDirectory>>> {
        self.directory.as_ref().map(|d| &d.0)
    }
    fn prepared(&self) -> Result<(), IndexedRelayError> {
        let now = unix_now();
        self.office
            .state
            .request_preparation_at(now)
            .map_err(IndexedRelayError::from)?;
        if !self
            .office
            .state
            .preparation_ready_at(now)
            .map_err(IndexedRelayError::from)?
        {
            return Err(IndexedRelayError::PreparationPending);
        }
        Ok(())
    }
    /// Preparation is demanded outside the connection lifetime. A miss schedules
    /// bounded worker work; this function never hashes a whole mailbox inline.
    pub async fn sync_with_relay_checked(
        &self,
        addr: SocketAddr,
        identity: &DeviceIdentity,
        expected: &PublicIdentity,
    ) -> Result<usize, IndexedRelayError> {
        self.prepared()?;
        let started = Instant::now();
        let network = match &self.trust {
            Some(IndexedRelayTrust::Network(network)) => Some(network),
            _ => None,
        };
        tokio::time::timeout(ROUND_TIMEOUT, async {
            let versions: &[&[u8]] = if network.is_some() {
                &[REQUEST_V4, REQUEST_V3, REQUEST_V2]
            } else {
                &[REQUEST_V4, REQUEST_V3, REQUEST_V2, REQUEST]
            };
            for request in versions {
                match self
                    .sync_version(addr, identity, expected, request, network, started)
                    .await
                {
                    Err(IndexedRelayError::UnsupportedVersion) => continue,
                    result => return result,
                }
            }
            Err("unsupported federation version".into())
        })
        .await
        .map_err(|_| "federation exchange deadline exceeded".to_string())?
    }
    async fn sync_version(
        &self,
        addr: SocketAddr,
        identity: &DeviceIdentity,
        expected: &PublicIdentity,
        request: &[u8],
        network: Option<&super::super::network::RelayNetwork>,
        started: Instant,
    ) -> Result<usize, IndexedRelayError> {
        let owner = SessionOwner::begin(
            self.office.state.profile().clone(),
            Direction::Outbound,
            unix_now(),
            started,
        )
        .map_err(IndexedRelayError::from)?;
        let lease = owner.lease();
        let stream = TcpStream::connect(addr)
            .await
            .map_err(IndexedRelayError::from)?;
        stream.set_nodelay(true).map_err(IndexedRelayError::from)?;
        let mut channel = SecureChannel::connect(stream, identity, Some(expected))
            .await
            .map_err(IndexedRelayError::from)?;
        if let Some(network) = network {
            network.connect(&mut channel).await?;
        }
        send_frame(&mut channel, request).await?;
        let limit = presence_limit(request);
        let routed = request == REQUEST_V3 || request == REQUEST_V4;
        let origins = request == REQUEST_V4;
        send_presence_pages(&mut channel, self.directory(), limit, routed, origins).await?;
        let view = self
            .office
            .state
            .inventory_view_at(unix_now())
            .map_err(IndexedRelayError::from)?;
        self.send_inventory(&mut channel, &lease, &view).await?;
        receive_inventory(&mut channel, &lease).await?;
        self.correct_origins(&lease).await?;
        receive_presence_pages(&mut channel, self.directory(), limit, routed, origins).await?;
        self.office
            .state
            .begin_inventory_mismatch_at(
                &view,
                &lease,
                self.office.state.federation_cursor(expected).as_ref(),
                unix_now(),
            )
            .map_err(IndexedRelayError::from)?;
        let mut ids = Vec::with_capacity(MAX_MAILBOXES_PER_EXCHANGE);
        loop {
            let step = self
                .office
                .state
                .inventory_mismatch_step_at(
                    &view,
                    &lease,
                    512,
                    MAX_MAILBOXES_PER_EXCHANGE - ids.len(),
                    unix_now(),
                )
                .map_err(IndexedRelayError::from)?;
            ids.extend(step.ids);
            if step.done || ids.len() == MAX_MAILBOXES_PER_EXCHANGE {
                break;
            }
            // A raw empty bounded slice is progress, never convergence. Yield so
            // the absolute exchange deadline/cancellation can interrupt a scan.
            tokio::task::yield_now().await;
        }
        let store = Mutex::new(IndexedFederationStore::new(&self.office, lease));
        for id in &ids {
            self.office.state.advance_federation_cursor(expected, *id);
            request_round_budgeted(&mut channel, &store, *id, &self.budget)
                .await
                .map_err(IndexedRelayError::from)?;
        }
        // `owner` remains live through every round and transport await. A cloned
        // store/view/lease cannot extend this authority after return/cancellation.
        drop(owner);
        Ok(ids.len())
    }
    async fn correct_origins(&self, lease: &SessionLease) -> Result<(), IndexedRelayError> {
        let mut after = None;
        loop {
            let step = self
                .office
                .state
                .correct_known_origins_at(lease, after.as_ref(), 512, unix_now())
                .map_err(IndexedRelayError::from)?;
            log::trace!("relay origin correction examined {} keys", step.examined);
            after = step.after;
            if after.is_none() {
                return Ok(());
            }
            tokio::task::yield_now().await;
        }
    }
    /// Two bounded emitted page buffers at most. Raw empty/nonfederated slices
    /// are coalesced before frame accounting. One bounded lookahead determines
    /// terminality even at the exact 256-frame ceiling.
    async fn send_inventory<IO: AsyncRead + AsyncWrite + Unpin>(
        &self,
        channel: &mut SecureChannel<IO>,
        lease: &SessionLease,
        view: &InventoryView,
    ) -> Result<(), IndexedRelayError> {
        let mut after = None;
        let mut done = false;
        let mut frame = Vec::with_capacity(IDS_PER_PAGE);
        loop {
            while frame.len() < IDS_PER_PAGE && !done {
                let page = self
                    .office
                    .state
                    .selected_inventory_page_at(
                        view,
                        lease,
                        after.as_ref(),
                        IDS_PER_PAGE - frame.len(),
                        unix_now(),
                    )
                    .map_err(IndexedRelayError::from)?;
                log::trace!("relay inventory examined {} keys", page.examined);
                frame.extend(page.entries);
                after = page.after;
                done = after.is_none();
                tokio::task::yield_now().await;
            }
            let mut next = Vec::new();
            while frame.len() == IDS_PER_PAGE && !done && next.is_empty() {
                let page = self
                    .office
                    .state
                    .selected_inventory_page_at(
                        view,
                        lease,
                        after.as_ref(),
                        IDS_PER_PAGE,
                        unix_now(),
                    )
                    .map_err(IndexedRelayError::from)?;
                next = page.entries;
                after = page.after;
                done = after.is_none();
                tokio::task::yield_now().await;
            }
            let last = done && next.is_empty();
            let bytes = encode_page(&frame, last)?;
            self.office
                .state
                .finish_inventory_frame_at(view, lease, frame.len(), last, unix_now())
                .map_err(IndexedRelayError::from)?;
            send_frame(channel, &bytes).await?;
            if last {
                return Ok(());
            }
            frame = next;
        }
    }
    /// Authenticated Noise is necessary but federation additionally requires
    /// configured pins or successful mutual membership before metadata access.
    pub async fn serve_connection_checked<IO: AsyncRead + AsyncWrite + Unpin>(
        &self,
        mut channel: SecureChannel<IO>,
        origin_signer: Option<&DeviceIdentity>,
    ) -> Result<(), IndexedRelayError> {
        let started = Instant::now();
        let deadline = tokio::time::Instant::from_std(started + ROUND_TIMEOUT);
        let mut first = tokio::time::timeout_at(deadline, channel.recv())
            .await
            .map_err(|_| "relay first-frame deadline exceeded".to_string())?
            .map_err(IndexedRelayError::from)?;
        let mut network_authorized = false;
        if super::super::network::RelayNetwork::is_membership_request(&first) {
            let Some(IndexedRelayTrust::Network(network)) = &self.trust else {
                return Err("relay network membership is not enabled".into());
            };
            tokio::time::timeout_at(deadline, network.accept(&mut channel, &first))
                .await
                .map_err(|_| "federation authorization deadline exceeded".to_string())??;
            first = tokio::time::timeout_at(deadline, channel.recv())
                .await
                .map_err(|_| "federation authorization deadline exceeded".to_string())?
                .map_err(IndexedRelayError::from)?;
            if !is_inventory_request(&first) {
                return Err("expected authorized federation inventory request".into());
            }
            network_authorized = true;
        }
        if is_inventory_request(&first) {
            if !network_authorized
                && !matches!(&self.trust, Some(IndexedRelayTrust::Pinned(pins)) if pins.contains(&channel.peer_identity().user_id()))
            {
                return Err("federation peer is not authorized".into());
            }
            return tokio::time::timeout_at(
                deadline,
                self.serve_federation(&mut channel, &first, started),
            )
            .await
            .map_err(|_| "federation exchange deadline exceeded".to_string())?;
        }
        if crate::node::bridge_directory::is_request(&first) {
            return self
                .serve_directory(&mut channel, &first, origin_signer)
                .await;
        }
        let store = Mutex::new(self.office.clone());
        match tokio::time::timeout(
            ROUND_TIMEOUT,
            serve_wire_bytes_budgeted(&mut channel, &store, &first, &self.budget),
        )
        .await
        {
            Ok(Ok(Served::Handled(_))) => {}
            Ok(Ok(Served::Closed)) => return Ok(()),
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => return Err("relay round deadline exceeded".into()),
        }
        for _ in 1..10_000 {
            match tokio::time::timeout(
                ROUND_TIMEOUT,
                serve_one_budgeted(&mut channel, &store, &self.budget),
            )
            .await
            {
                Ok(Ok(Served::Handled(_))) => {}
                Ok(Ok(Served::Closed)) => return Ok(()),
                Ok(Err(error)) => return Err(error.into()),
                Err(_) => return Err("relay round deadline exceeded".into()),
            }
        }
        Ok(())
    }
    async fn serve_federation<IO: AsyncRead + AsyncWrite + Unpin>(
        &self,
        channel: &mut SecureChannel<IO>,
        request: &[u8],
        started: Instant,
    ) -> Result<(), IndexedRelayError> {
        self.prepared()?;
        let owner = SessionOwner::begin(
            self.office.state.profile().clone(),
            Direction::Inbound,
            unix_now(),
            started,
        )
        .map_err(IndexedRelayError::from)?;
        let lease = owner.lease();
        let limit = presence_limit(request);
        let routed = request == REQUEST_V3 || request == REQUEST_V4;
        let origins = request == REQUEST_V4;
        receive_presence_pages(channel, self.directory(), limit, routed, origins).await?;
        receive_inventory(channel, &lease).await?;
        self.correct_origins(&lease).await?;
        let view = self
            .office
            .state
            .inventory_view_at(unix_now())
            .map_err(IndexedRelayError::from)?;
        self.send_inventory(channel, &lease, &view).await?;
        send_presence_pages(channel, self.directory(), limit, routed, origins).await?;
        let store = Mutex::new(IndexedFederationStore::new(&self.office, lease));
        for _ in 0..10_000 {
            match serve_one_budgeted(channel, &store, &self.budget)
                .await
                .map_err(IndexedRelayError::from)?
            {
                Served::Closed => break,
                Served::Handled(_) => {}
            }
        }
        drop(owner);
        Ok(())
    }
    async fn serve_directory<IO: AsyncRead + AsyncWrite + Unpin>(
        &self,
        channel: &mut SecureChannel<IO>,
        request: &[u8],
        origin_signer: Option<&DeviceIdentity>,
    ) -> Result<(), IndexedRelayError> {
        let Some(directory) = self.directory() else {
            return Ok(());
        };
        let response = {
            let mut directory = directory
                .lock()
                .map_err(|e| IndexedRelayError::Protocol(e.to_string()))?;
            let peers = directory.register(
                channel.peer_identity(),
                channel.peer_announcement(),
                channel.peer_publicly_visible(),
                Instant::now(),
            );
            peers.and_then(|peers| {
                if !directory.apply_origin_request(
                    request,
                    channel.peer_identity(),
                    origin_signer,
                    unix_now(),
                ) {
                    return None;
                }
                directory.encode_snapshot(request, &peers)
            })
        };
        if let Some(pages) = response {
            for page in pages {
                send_frame(channel, &page).await?;
            }
        }
        Ok(())
    }
}
async fn receive_inventory<IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    lease: &SessionLease,
) -> Result<(), IndexedRelayError> {
    for page_number in 0..MAX_PAGES {
        let bytes = channel.recv().await.map_err(|error| {
            if page_number == 0 && matches!(&error, crate::transport::TransportError::Io(io)
                if matches!(io.kind(), std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset)) {
                IndexedRelayError::UnsupportedVersion
            } else { error.into() }
        })?;
        let (entries, last) = decode_page(&bytes)?;
        lease
            .receive_page(&entries, last, unix_now())
            .map_err(IndexedRelayError::from)?;
        if last {
            return Ok(());
        }
    }
    Err("incomplete or oversized federation inventory".into())
}

#[cfg(test)]
mod tests;
