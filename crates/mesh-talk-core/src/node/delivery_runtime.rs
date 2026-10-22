//! Atomic DM acceptance and control receipt dispatch. Network hooks take owned
//! immutable snapshots; no delivery or ratchet guard crosses a transport await.
use super::delivery_store::{DeliveryDestination, ReceiptDelivery};
use super::delivery_store::{DeliveryStore, DeliveryTransaction, OutgoingDelivery};
use super::sentlog::SentEntry;
use super::session::SessionError;
use super::*;
use crate::eventlog::{ConversationId, Event, EventId, LogError};
use crate::identity::device::PublicIdentity;

// Owned by the delivery loop: cancellation drops every retained channel. Keep
// eight scheduling slots, at most six retained sockets (one outbound permit
// reserved for controls), and discard idle or unauthorized state.
const MAX_FILE_TRANSFERS: usize = 8;
const MAX_FILE_SOCKETS: usize = super::session_pool::MAX_DIRECTION_CONNECTIONS - 1;
const MAX_RELAY_CUSTODY_HINTS: usize = 256;
const FILE_TRANSFER_IDLE: std::time::Duration = std::time::Duration::from_secs(30);
const FILE_CONSENT_RETRY: std::time::Duration = std::time::Duration::from_secs(3);
pub(in crate::node) const FILE_EXCHANGE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(15);
pub(in crate::node) const RELAY_EXCHANGE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(5);
const RELAY_FAILURE_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(10);
const FILE_RELAY_RETRY: std::time::Duration = std::time::Duration::from_secs(10);

// Whole authenticated control exchanges need several routed round trips. Keep
// them off the serial file scheduler, with independent capacity for each lane.
const CONTROL_EXCHANGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum ControlWork {
    Destination(EventId),
    Receipt(EventId),
    Pull(String),
}

struct ControlRoutes {
    peer: PublicIdentity,
    endpoints: std::collections::BTreeSet<(String, std::net::SocketAddr)>,
    publicly_observed: bool,
}

impl ControlWork {
    fn retry_key(&self, peer: &PublicIdentity) -> (u8, String) {
        (
            match self {
                Self::Destination(_) => 0,
                Self::Receipt(_) => 1,
                Self::Pull(_) => 2,
            },
            peer.user_id(),
        )
    }
}

impl ControlRoutes {
    fn retry_routes(
        &self,
    ) -> (
        std::collections::BTreeSet<(String, std::net::SocketAddr)>,
        bool,
    ) {
        (self.endpoints.clone(), self.publicly_observed)
    }
}

// Test-only timing: a dropped span records cancellation/early return, not success.
#[cfg(test)]
pub(super) struct TestTiming {
    id: u64,
    started: std::time::Instant,
    phase: &'static str,
    enabled: bool,
    finished: bool,
    records: Vec<(&'static str, u128, u128)>,
    ports: Option<(u16, u16)>,
    omitted: usize,
}

#[cfg(test)]
impl TestTiming {
    pub(super) fn new(phase: &'static str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let mut timing = Self {
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            started: std::time::Instant::now(),
            phase,
            enabled: std::env::var("MESH_TALK_TEST_TIMING").as_deref() == Ok("1"),
            finished: false,
            records: Vec::new(),
            ports: None,
            omitted: 0,
        };
        timing.mark(phase);
        timing
    }

    pub(super) fn mark(&mut self, phase: &'static str) {
        if self.enabled {
            static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
            if self.records.len() == 32 {
                self.records.remove(0);
                self.omitted += 1;
            }
            self.records.push((
                phase,
                self.started.elapsed().as_micros(),
                EPOCH
                    .get_or_init(std::time::Instant::now)
                    .elapsed()
                    .as_micros(),
            ));
        }
    }

    pub(super) fn phase(&mut self, phase: &'static str) {
        self.phase = phase;
        self.mark(phase);
    }

    pub(super) fn finish(&mut self) {
        self.mark("complete");
        self.finished = true;
    }

    pub(super) fn socket(&mut self, stream: &tokio::net::TcpStream) {
        if self.enabled
            && let (Ok(local), Ok(remote)) = (stream.local_addr(), stream.peer_addr())
        {
            self.ports = Some((local.port(), remote.port()));
        }
    }
}

#[cfg(test)]
impl Drop for TestTiming {
    fn drop(&mut self) {
        if !self.finished {
            self.mark("dropped");
        }
        if self.enabled {
            use std::fmt::Write;
            let mut output = String::new();
            let _ = writeln!(
                output,
                "test-timing id={} finished={} last_phase={} omitted={} ports={:?}",
                self.id, self.finished, self.phase, self.omitted, self.ports
            );
            for (phase, elapsed, tick) in &self.records {
                let _ = writeln!(
                    output,
                    "test-timing id={} phase={phase} elapsed_us={elapsed} tick_us={tick}",
                    self.id
                );
            }
            // One stderr lock per bounded span, never one per protocol phase.
            eprint!("{output}");
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum FilePhase {
    Dial,
    Manifest,
    ManifestCustody,
    Consent,
    ConsentWait,
    InitialCustody,
    Chunks,
    FinalCustody,
    RelayManifestLegacy,
    RelayManifestFederated,
    RelayChunksLegacy,
    RelayChunksFederated,
    RelayCustody,
    RelayHeld,
}

pub(in crate::node) struct FileTransfer {
    file: super::delivery_store::FileCard,
    destination: super::delivery_store::FileDestination,
    channel: Option<crate::transport::SecureChannel<tokio::net::TcpStream>>,
    phase: FilePhase,
    sync: super::session::SyncProgress,
    legacy: bool,
    last_used: std::time::Instant,
    candidate_cursor: usize,
    candidate_count: usize,
    route: Option<(bool, crate::discovery::roster::PeerRecord)>,
    opaque: Option<super::opaque_relay::OpaqueMailboxProgress>,
    relay_custody: Option<(ConversationId, EventId)>,
}

impl FileTransfer {
    fn new(
        file: super::delivery_store::FileCard,
        destination: super::delivery_store::FileDestination,
    ) -> Self {
        Self {
            file,
            destination,
            channel: None,
            phase: FilePhase::Dial,
            sync: super::session::SyncProgress::default(),
            legacy: false,
            last_used: std::time::Instant::now(),
            candidate_cursor: 0,
            candidate_count: 0,
            route: None,
            opaque: None,
            relay_custody: None,
        }
    }
}

type FileKey = (EventId, EventId);
type FileStepResult = (
    FileKey,
    std::collections::HashMap<FileKey, FileTransfer>,
    bool,
);
type FileWork = (
    super::delivery_store::FileCard,
    super::delivery_store::FileDestination,
);

/// The worker owns file scheduling state across wakeups. A scan takes only a
/// cursor and a capacity snapshot, so no socket or mutable cache crosses the
/// blocking journal read.
#[derive(Default)]
struct FileDeliveryScheduler {
    pending_cursor: Option<FileKey>,
    active_cursor: Option<FileKey>,
    transfers: std::collections::HashMap<FileKey, FileTransfer>,
    workers: tokio::task::JoinSet<FileStepResult>,
    running_sockets: std::collections::HashSet<FileKey>,
    // Bounded scheduling hints, not custody/delivery truth. Eviction merely
    // permits an earlier retry after other destinations have had their turn.
    relay_cooldowns: std::collections::VecDeque<(FileKey, std::time::Instant, bool)>,
}

#[derive(Clone, Copy)]
struct FileScan {
    cursor: Option<FileKey>,
    slots: usize,
}

impl FileScan {
    fn read(self, store: &DeliveryStore) -> (Vec<FileWork>, Option<FileKey>) {
        let mut files = Vec::with_capacity(self.slots);
        let mut cursor = self.cursor;
        for _ in 0..self.slots {
            let Some((file, destination)) = store.next_file_destination(cursor) else {
                break;
            };
            let key = (file.id, destination.binding.event_id);
            if files.iter().any(|(previous, reference): &FileWork| {
                (previous.id, reference.binding.event_id) == key
            }) {
                break;
            }
            cursor = Some(key);
            files.push((file, destination));
        }
        (files, cursor)
    }
}

impl FileDeliveryScheduler {
    fn scan(&mut self, node: &Node) -> FileScan {
        node.prune_file_transfers(&mut self.transfers);
        self.relay_cooldowns.retain(|(key, until, consent)| {
            *until > std::time::Instant::now()
                && (!*consent
                    || node.outgoing_file_consent_status(key.0, key.1)
                        != Some(crate::eventlog::sync::FileConsentStatus::Accepted))
        });
        node.prune_outgoing_file_acks();
        FileScan {
            cursor: self.pending_cursor,
            slots: MAX_FILE_TRANSFERS - self.transfers.len() - self.workers.len(),
        }
    }

    fn admit(&mut self, node: &Node, files: Vec<FileWork>, cursor: Option<FileKey>) {
        self.pending_cursor = cursor;
        node.prune_file_transfers(&mut self.transfers);
        for (file, destination) in files {
            let key = (file.id, destination.binding.event_id);
            if self.transfers.len() + self.workers.len() < MAX_FILE_TRANSFERS
                && !self.relay_cooldowns.iter().any(|(held, _, _)| *held == key)
                && node.file_destination_current(&file, &destination)
                && !self.transfers.contains_key(&key)
                && !self.running_sockets.contains(&key)
            {
                self.transfers
                    .insert(key, FileTransfer::new(file, destination));
            }
        }
    }

    fn advance(&mut self, node: &std::sync::Arc<Node>) {
        // Each exchange owns its socket until a clean protocol boundary. Slow
        // peers consume one bounded worker, never the serial delivery loop.
        let mut keys = self.transfers.keys().copied().collect::<Vec<_>>();
        keys.sort_unstable();
        if let Some(cursor) = self.active_cursor {
            let offset = keys.partition_point(|key| *key <= cursor);
            keys.rotate_left(offset);
        }
        let mut sockets = self.running_sockets.len()
            + self
                .transfers
                .values()
                .filter(|t| t.channel.is_some())
                .count();
        for key in keys {
            let transfer = &self.transfers[&key];
            let dial = matches!(transfer.phase, FilePhase::Dial);
            if dial && sockets >= MAX_FILE_SOCKETS {
                continue;
            }
            if dial {
                sockets += 1;
            }
            self.active_cursor = Some(key);
            self.running_sockets.insert(key);
            #[cfg(test)]
            node.file_transfer_peak.fetch_max(
                self.transfers.len() + self.workers.len(),
                std::sync::atomic::Ordering::SeqCst,
            );
            let transfer = self.transfers.remove(&key).unwrap();
            let file = transfer.file.clone();
            let destination = transfer.destination.clone();
            let node = node.clone();
            let Some(work) = node.runtime_work.admit() else {
                return;
            };
            self.workers.spawn(work.track(async move {
                let mut transfers = std::collections::HashMap::from([(key, transfer)]);
                let complete = node
                    .retry_file_step(&file, &destination, &mut transfers)
                    .await;
                (key, transfers, complete)
            }));
        }
    }

    fn finish(
        &mut self,
        node: &Node,
        result: Result<FileStepResult, tokio::task::JoinError>,
        diagnostic: &mut DeliveryDiagnosticThrottle,
    ) {
        let Ok((key, mut transfers, complete)) = result else {
            // Never retain reservations after a panicked child; drop cancels the
            // other workers and the owning accept loop retires on this failure.
            self.running_sockets.clear();
            self.workers.abort_all();
            return;
        };
        self.running_sockets.remove(&key);
        if complete {
            node.delivery_notify.notify_one();
            if node
                .delivery
                .lock()
                .expect("delivery lock not poisoned")
                .retire_file_destination(key.0, key.1)
                .is_err()
            {
                diagnostic.warn_if_due();
            } else {
                node.outgoing_file_acks
                    .lock()
                    .expect("outgoing file progress lock not poisoned")
                    .remove(&key);
                node.note_file_change();
            }
        }
        if let Some(transfer) = transfers.remove(&key) {
            let delay = match transfer.phase {
                FilePhase::RelayHeld => Some(FILE_RELAY_RETRY),
                FilePhase::ConsentWait => Some(FILE_CONSENT_RETRY),
                _ => None,
            };
            if let Some(delay) = delay {
                if self.relay_cooldowns.len() == MAX_RELAY_CUSTODY_HINTS {
                    self.relay_cooldowns.pop_front();
                }
                self.relay_cooldowns.push_back((
                    key,
                    std::time::Instant::now() + delay,
                    matches!(transfer.phase, FilePhase::ConsentWait),
                ));
                node.delivery_notify.notify_one();
            } else {
                self.transfers.insert(key, transfer);
                node.delivery_notify.notify_one();
            }
        }
    }
}

#[derive(Default)]
struct DeliveryDiagnosticThrottle {
    last_emitted: Option<std::time::Instant>,
}

impl DeliveryDiagnosticThrottle {
    fn admit(&mut self, now: std::time::Instant) -> bool {
        if self.last_emitted.is_some_and(|last| {
            now.saturating_duration_since(last) < std::time::Duration::from_secs(30)
        }) {
            return false;
        }
        self.last_emitted = Some(now);
        true
    }

    fn warn_if_due(&mut self) {
        if self.admit(std::time::Instant::now()) {
            log::warn!("automatic delivery local persistence deferred");
        }
    }
}

// The delivery-loop lifetime owns all transient observations. Every control
// task belongs to its JoinSet; cancellation cannot detach a hint producer.
struct ObservationScope(std::sync::Arc<Node>);
impl Drop for ObservationScope {
    fn drop(&mut self) {
        self.0.clear_delivery_observations();
    }
}

pub(super) struct ObservationAttempt {
    pub(super) node: std::sync::Arc<Node>,
    pub(super) token: Option<super::delivery_observation::AttemptToken>,
}
impl Drop for ObservationAttempt {
    fn drop(&mut self) {
        if let Some(token) = self.token {
            self.node
                .delivery_observations
                .lock()
                .expect("delivery observations lock not poisoned")
                .cancel(token);
        }
    }
}

impl Node {
    /// Replace the exact device fingerprints allowed for proactive DM recovery.
    /// Hosts populate this from an explicit safety-number verification decision;
    /// incoming pushes remain available to other authenticated peers.
    pub fn replace_verified_dm_pull_peers(&self, fingerprints: Vec<String>) {
        *self
            .verified_dm_pull_peers
            .lock()
            .expect("verified DM pull mutex not poisoned") = fingerprints
            .into_iter()
            .map(|fingerprint| fingerprint.to_ascii_lowercase())
            .collect();
        self.delivery_notify.notify_one();
    }

    pub(in crate::node) fn verified_dm_pull_peer(&self, fingerprint: &str) -> bool {
        self.verified_dm_pull_peers
            .lock()
            .expect("verified DM pull mutex not poisoned")
            .contains(fingerprint)
            || (self.bridge_directory_allowed()
                && self
                    .bridge_dm_pull_peers
                    .lock()
                    .expect("bridge DM pull mutex not poisoned")
                    .get(fingerprint)
                    .is_some_and(|seen| seen.elapsed() < std::time::Duration::from_secs(45)))
    }

    /// Authorize one immutable outgoing journal entry, then attempt local replay.
    /// Once the journal accepts it, replay failure must not become a retryable send.
    pub(in crate::node) fn commit_outgoing_transaction(
        &self,
        store: &mut DeliveryStore,
        transaction: DeliveryTransaction,
    ) -> Result<bool, NodeError> {
        // The caller encloses preparation and this WAL append in host authority.
        // Never acquire host authority while retaining the journal mutex.
        self.log
            .lock()
            .expect("log lock")
            .sync()
            .map_err(NodeError::Log)?;
        store.begin(transaction).map_err(NodeError::Log)?;
        let recovered = self.recover_delivery(store).is_ok();
        self.delivery_notify.notify_one();
        Ok(recovered)
    }

    pub(in crate::node) fn accept_manifest_event(
        &self,
        event: &Event,
        proof: &crate::discovery::Announce,
        received: ReceivedEntry,
    ) -> Result<(), LogError> {
        let mut store = self.delivery.lock().expect("delivery lock not poisoned");
        self.recover_delivery(&mut store)?;
        let own = self.identity.public();
        let peer = proof.public();
        if !proof.verify()
            || event.author.ed25519_pub() != &peer.ed25519_pub
            || event.conversation_id != super::conversation::dm_conversation_id(&own, &peer)
        {
            return Err(LogError::CorruptFile("invalid manifest device pair".into()));
        }
        let manifest = super::files::validated_manifest(&received.plaintext)
            .ok_or_else(|| LogError::CorruptFile("invalid file manifest".into()))?;
        if store.manifest_event_erased(event.id)
            || manifest.file_conv() == event.conversation_id
            || manifest.file_conv() == received.conversation
            || self
                .files
                .lock()
                .expect("files lock not poisoned")
                .manifest(&manifest.file_conv())
                .is_some_and(|existing| !super::files::same_file_transfer(existing, &manifest))
        {
            return Err(LogError::CorruptFile(
                "conflicting immutable file manifest".into(),
            ));
        }
        {
            let mut permitted_authors = self
                .files
                .lock()
                .expect("files lock not poisoned")
                .authors(manifest.file_conv());
            permitted_authors.insert(event.author);
            let log = self.log.lock().expect("log lock not poisoned");
            let mut chunks = log.events(&manifest.file_conv());
            chunks.sort_by_key(|chunk| chunk.seq);
            let mut previous = None;
            let mut chunk_author = None;
            if chunks.iter().enumerate().any(|(index, chunk)| {
                let mixed_authors = chunk_author.is_some_and(|author| author != chunk.author);
                chunk_author = Some(chunk.author);
                let malformed_chain = chunk.seq != index as u64 + 1
                    || match previous {
                        None => !chunk.parents.is_empty(),
                        Some(id) => chunk.parents != [id],
                    };
                previous = Some(chunk.id);
                malformed_chain
                    || mixed_authors
                    || chunk.kind != crate::eventlog::EventKind::Message
                    || !permitted_authors.contains(&chunk.author)
                    || chunk.seq == 0
                    || chunk.seq > u64::from(manifest.chunk_count())
                    || !chunk.verify_integrity()
                    || !chunk.verify_signature()
                    || crate::file::open_chunk_for(
                        &manifest,
                        (chunk.seq - 1) as u32,
                        &chunk.ciphertext,
                    )
                    .is_err()
            }) {
                return Err(LogError::CorruptFile(
                    "conflicting file chunk conversation".into(),
                ));
            }
        }
        let payload = super::delivery_receipt::ReceiptPayload::prepare(
            &own,
            &self.account_id(),
            proof,
            event,
            &received,
            super::node::now_millis(),
        );
        let receipt = if let Some(payload) = payload {
            if store.has_receipt_for(received.conversation, event.id) {
                return Ok(());
            }
            let log = self.log.lock().expect("log lock not poisoned");
            let conv = payload.conversation();
            let (_, lamport) = log.prepare(&conv)?;
            let author = crate::eventlog::Author::from_ed25519(own.ed25519_pub);
            let seq = log.next_sequence(&conv, &author)?;
            let wire = payload
                .seal(&self.identity)
                .map_err(|_| LogError::CorruptFile("manifest receipt sealing failed".into()))?;
            let control = Event::new(
                &self.identity,
                conv,
                seq,
                vec![],
                lamport,
                payload.confirmed_at(),
                crate::eventlog::EventKind::Message,
                wire,
            );
            Some(Box::new(ReceiptDelivery {
                logical_id: event.id,
                original_event_id: event.id,
                conversation: received.conversation,
                wall_clock: event.wall_clock,
                destination: DeliveryDestination {
                    device: peer.clone(),
                    account: proof.account_id(),
                    event: control,
                    receipt_eligible: false,
                },
            }))
        } else {
            None
        };
        self.log.lock().expect("log lock not poisoned").sync()?;
        let file = super::delivery_store::FileCard::incoming(
            event,
            &received,
            &manifest,
            super::delivery_store::FileCompletionBinding {
                source: peer.clone(),
                certificate: proof.account_cert.clone(),
                owner_account: self.account_id(),
            },
        );
        store.begin(DeliveryTransaction::IncomingManifest {
            sender: peer,
            original: Box::new(event.clone()),
            received: Box::new(received),
            receipt,
            file,
        })?;
        self.recover_delivery(&mut store)?;
        self.delivery_notify.notify_one();
        Ok(())
    }
    pub(in crate::node) fn persist_account_adoption<T>(
        &self,
        account: &str,
        save: impl FnOnce() -> Result<T, NodeError>,
    ) -> Result<T, NodeError> {
        let mut store = self.delivery.lock().expect("delivery lock not poisoned");
        self.recover_delivery(&mut store).map_err(NodeError::Log)?;
        store
            .prepare_profile_adoption(account)
            .map_err(NodeError::Log)?;
        match save() {
            Ok(result) => {
                self.delivery_suspended
                    .store(true, std::sync::atomic::Ordering::Release);
                Ok(result)
            }
            Err(error) => {
                if store.cancel_profile_adoption().is_err() {
                    log::warn!("account adoption marker cleanup deferred");
                }
                Err(error)
            }
        }
    }
    pub(in crate::node) fn delivery_destination_allowed(
        &self,
        destination: &DeliveryDestination,
    ) -> bool {
        let sibling_allowed = if self.certified_sibling(&destination.device) {
            let policy = self.device_sync_policy();
            match destination.event.kind {
                crate::eventlog::EventKind::Message => {
                    destination.event.conversation_id
                        == super::delivery_receipt::delivery_conversation_id(
                            &self.identity.public(),
                            &destination.device,
                        )
                        || policy.history
                }
                crate::eventlog::EventKind::FileManifest => policy.attachments,
                _ => true,
            }
        } else {
            true
        };
        sibling_allowed
            && !self
                .delivery_suspended
                .load(std::sync::atomic::Ordering::Acquire)
            && self
                .historical_author(&destination.device.ed25519_pub)
                .is_some_and(|p| {
                    p.public() == destination.device
                        && (destination.account.is_none() || p.account_id() == destination.account)
                })
    }
    pub(in crate::node) fn accept_dm_event(
        &self,
        event: &Event,
        proof: &crate::discovery::Announce,
        store: &mut DeliveryStore,
    ) -> Option<()> {
        self.accept_dm_event_with_author(event, &proof.public(), Some(proof), store)
    }

    pub(in crate::node) fn accept_dm_event_with_author(
        &self,
        event: &Event,
        public: &PublicIdentity,
        proof: Option<&crate::discovery::Announce>,
        store: &mut DeliveryStore,
    ) -> Option<()> {
        let peer = public.clone();
        let sibling = self.certified_sibling(&peer);
        // Offline replay can use the already persisted exact sibling certificate.
        // Receipt and control processing still require the real signed announcement.
        if proof.is_none() && !sibling {
            return None;
        }
        let own = self.identity.public();
        if event.conversation_id == super::delivery_receipt::delivery_conversation_id(&own, &peer) {
            self.process_delivery_control(event, proof?, store);
            return None;
        }
        if event.conversation_id != super::conversation::dm_conversation_id(&own, &peer)
            || event.author.ed25519_pub() != &peer.ed25519_pub
            || !event.verify_integrity()
            || !event.verify_signature()
            || event.ciphertext.len() > 256 * 1024
            || event.parents.len() > 1024
        {
            return None;
        }
        if sibling && !self.device_sync_policy().history {
            return None;
        }
        store
            .check_capacity(1, event.ciphertext.len() as u64)
            .ok()?;
        let (plain, prepared) = self
            .dm_ratchet
            .lock()
            .expect("ratchet lock not poisoned")
            .prepare_decrypt(&self.identity, &peer, &event.ciphertext)
            .ok()?;
        if plain.len() > 256 * 1024 {
            return None;
        }
        let account = self.account_id();
        let sender_account = match proof {
            Some(proof) => proof.account_id(),
            None => Some(account.clone()),
        };
        let (conversation, from) = if plain.starts_with(b"MTDE1") {
            let env = DmEnvelope::decode(&plain)?;
            let valid_account = |id: &str| {
                id.len() == 32
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            };
            if !valid_account(&env.route.sender_account)
                || !valid_account(&env.route.recipient_account)
                || sender_account.as_deref() != Some(env.route.sender_account.as_str())
                || (env.route.sender_account != account && env.route.recipient_account != account)
            {
                return None;
            }
            (
                super::conversation::account_conversation_id(
                    &env.route.sender_account,
                    &env.route.recipient_account,
                ),
                env.route.sender_account,
            )
        } else {
            (event.conversation_id, peer.user_id())
        };
        let received = ReceivedEntry {
            event_id: event.id,
            conversation,
            from,
            wall_clock: event.wall_clock,
            plaintext: plain,
        };
        let payload = proof.and_then(|proof| {
            super::delivery_receipt::ReceiptPayload::prepare(
                &own,
                &account,
                proof,
                event,
                &received,
                super::node::now_millis(),
            )
        });
        let receipt = if let Some(payload) = payload {
            let proof = proof?;
            if store.has_receipt_for(conversation, event.id) {
                return None;
            }
            let wire = payload.seal(&self.identity).ok()?;
            let log = self.log.lock().expect("log lock not poisoned");
            let conv = payload.conversation();
            let (mut parents, lamport) = log.prepare(&conv).ok()?;
            // The Ed-pair scope survives account adoption and local deletion.
            // Never make a fresh receipt depend on control ancestry that the
            // current profile cannot safely project to this exact account.
            let controls = self
                .delivery_control_ids
                .lock()
                .expect("control ids lock not poisoned");
            let mut eligible = std::collections::HashSet::new();
            for previous in log.events(&conv) {
                let typed = (previous.author.ed25519_pub() == &own.ed25519_pub
                    && controls
                        .get(&previous.id)
                        .is_some_and(|b| b.device == peer && b.account == proof.account_id()))
                    || super::delivery_receipt::open_receipt(
                        &self.identity,
                        &account,
                        proof,
                        previous,
                    )
                    .is_some();
                if typed && previous.parents.iter().all(|id| eligible.contains(id)) {
                    eligible.insert(previous.id);
                }
            }
            parents.retain(|id| eligible.contains(id));
            drop(controls);
            let author = crate::eventlog::Author::from_ed25519(own.ed25519_pub);
            let seq = log.next_sequence(&conv, &author).ok()?;
            let control = Event::new(
                &self.identity,
                conv,
                seq,
                parents,
                lamport,
                payload.confirmed_at(),
                crate::eventlog::EventKind::Message,
                wire,
            );
            log.sync().ok()?;
            let logical_id = DmEnvelope::decode(&received.plaintext)
                .map_or(event.id, |env| EventId::new(env.msg_id));
            Some(Box::new(ReceiptDelivery {
                logical_id,
                original_event_id: event.id,
                conversation,
                wall_clock: event.wall_clock,
                destination: DeliveryDestination {
                    device: peer.clone(),
                    account: proof.account_id(),
                    event: control,
                    receipt_eligible: false,
                },
            }))
        } else {
            None
        };
        self.log
            .lock()
            .expect("log lock not poisoned")
            .sync()
            .ok()?;
        // Hold policy through durable acceptance; accepted WAL recovery remains unconditional.
        let policy = self
            .account_sync_policy
            .lock()
            .expect("account sync policy mutex not poisoned");
        if sibling && !policy.current().history {
            return None;
        }
        store
            .begin(DeliveryTransaction::Incoming {
                sender: peer.clone(),
                original: Box::new(event.clone()),
                ratchet: prepared,
                received: Box::new(received),
                receipt,
            })
            .ok()?;
        drop(policy);
        if self.recover_delivery(store).is_err() {
            log::warn!("accepted incoming delivery awaits local recovery");
            return None;
        }
        self.delivery_notify.notify_one();
        Some(())
    }
    pub(in crate::node) fn recover_delivery(
        &self,
        store: &mut DeliveryStore,
    ) -> Result<(), LogError> {
        if self
            .delivery_suspended
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(LogError::CorruptFile(
                "restart node after account adoption".into(),
            ));
        }
        store.validate_owner(&self.identity.public(), &self.account_id())?;
        store.sync_replacement()?;
        if store.pending_transactions().is_empty() {
            return Ok(());
        }
        loop {
            let notification = store
                .pending_transactions()
                .first()
                .and_then(|tx| match tx {
                    DeliveryTransaction::Incoming {
                        sender,
                        original,
                        received,
                        ..
                    } => Some((
                        sender.clone(),
                        original.conversation_id,
                        (**received).clone(),
                        false,
                        true,
                    )),
                    DeliveryTransaction::IncomingManifest {
                        sender,
                        original,
                        received,
                        ..
                    } => Some((
                        sender.clone(),
                        original.conversation_id,
                        (**received).clone(),
                        true,
                        true,
                    )),
                    DeliveryTransaction::OutgoingManifest { received, .. } => Some((
                        self.identity.public(),
                        received.conversation,
                        (**received).clone(),
                        true,
                        false,
                    )),
                    _ => None,
                });
            let wake_local =
                store
                    .pending_transactions()
                    .first()
                    .is_some_and(|tx| match tx.body() {
                        DeliveryTransaction::Outgoing { .. }
                        | DeliveryTransaction::OutgoingManifest { .. }
                        | DeliveryTransaction::ChannelOutgoing { .. } => true,
                        DeliveryTransaction::Incoming { sender, .. }
                        | DeliveryTransaction::IncomingManifest { sender, .. } => {
                            !self.certified_sibling(sender)
                        }
                        DeliveryTransaction::Requested { .. } => false,
                    });
            let transaction_id = store.pending_transactions().first().map(|tx| tx.id());
            if let Some(
                DeliveryTransaction::Incoming {
                    receipt: Some(receipt),
                    ..
                }
                | DeliveryTransaction::IncomingManifest {
                    receipt: Some(receipt),
                    ..
                },
            ) = store.pending_transactions().first()
            {
                self.delivery_control_ids
                    .lock()
                    .expect("control ids lock not poisoned")
                    .insert(
                        receipt.destination.event.id,
                        super::delivery_store::ControlBinding {
                            device: receipt.destination.device.clone(),
                            account: receipt.destination.account.clone(),
                        },
                    );
            }
            let channel_snapshot =
                store
                    .pending_transactions()
                    .first()
                    .and_then(|tx| match tx.body() {
                        DeliveryTransaction::ChannelOutgoing { sent, senders, .. } => {
                            Some((sent.conversation, senders.clone()))
                        }
                        _ => None,
                    });
            let installed = {
                let mut ratchet = self.dm_ratchet.lock().expect("ratchet lock not poisoned");
                let mut log = self.log.lock().expect("log lock not poisoned");
                let mut received = self.received.lock().expect("received lock not poisoned");
                let mut sent = self.sentlog.lock().expect("sent lock not poisoned");
                let mut files = self.received_files.lock().expect("files lock not poisoned");
                store.recover_next_with_channels(
                    &mut ratchet,
                    &mut log,
                    &mut sent,
                    &mut received,
                    Some(&mut files),
                    Some(&mut self.channel_senders.lock().expect("sender store lock")),
                )
            };
            let removed = transaction_id.is_some_and(|id| {
                store
                    .pending_transactions()
                    .first()
                    .is_none_or(|tx| tx.id() != id)
            });
            if (installed.is_ok() || removed)
                && let Some((channel, senders)) = channel_snapshot
                && let Some(state) = self
                    .channels
                    .lock()
                    .expect("channels lock")
                    .state_mut(&channel)
            {
                for (epoch, bytes) in senders {
                    if let Some(key) = crate::channel::sender_key::SenderKey::deserialize(&bytes) {
                        state.import_my_sender(epoch, key);
                    }
                }
            }
            if removed && wake_local {
                self.note_account_local_change();
            }
            if removed && let Some((sender, original_conv, entry, file, incoming)) = notification {
                self.note_account_history_change();
                if file {
                    if let Some(manifest) = crate::file::decode_manifest(&entry.plaintext) {
                        let fresh = {
                            let mut book = self.files.lock().expect("files lock not poisoned");
                            let fresh = !book.is_emitted(&entry.event_id);
                            book.mark_emitted(entry.event_id);
                            book.record_event(
                                entry.event_id,
                                crate::eventlog::Author::from_ed25519(sender.ed25519_pub),
                                manifest.clone(),
                            );
                            fresh
                        };
                        self.remember_manifest_scope(
                            manifest.file_conv(),
                            original_conv,
                            crate::eventlog::Author::from_ed25519(sender.ed25519_pub),
                            entry.event_id,
                        );
                        if !incoming {
                            self.install_file_card_scopes_for(store, Some(entry.event_id));
                        }
                        if incoming
                            && fresh
                            && !self.recipient_expired(
                                &entry.plaintext,
                                entry.event_id,
                                entry.wall_clock,
                            )
                        {
                            self.pending_files
                                .lock()
                                .expect("pending files lock not poisoned")
                                .insert(manifest.file_conv());
                            if !self.certified_sibling(&sender) {
                                let _ = self.file_incoming.send(ReceivedFile {
                                    expires_at_ms: crate::expiry::deadline(&entry.plaintext),
                                    conv: original_conv,
                                    is_channel: false,
                                    from: sender.user_id(),
                                    name: manifest.name().to_owned(),
                                    size: manifest.size(),
                                    mime: manifest.mime().to_owned(),
                                    file_conv: manifest.file_conv(),
                                    media: super::media_store::manifest_is_media(&manifest),
                                    event_id: entry.event_id,
                                    authored_at: entry.wall_clock,
                                    received_at: super::node::now_millis(),
                                    arrival_kind: "live",
                                    own_device: false,
                                    batch_id: None,
                                    batch_complete: true,
                                    batch_size: 1,
                                });
                            }
                        }
                    }
                } else if self
                    .emitted
                    .lock()
                    .expect("emitted lock not poisoned")
                    .insert(entry.event_id)
                {
                    let envelope = DmEnvelope::decode(&entry.plaintext);
                    let inner = envelope
                        .as_ref()
                        .map_or(entry.plaintext.as_slice(), |env| env.body.as_slice());
                    if self.recipient_expired(inner, entry.event_id, entry.wall_clock) {
                        continue;
                    }
                    let body = MessageBody::decode(inner);
                    let from_name = self
                        .historical_author(&sender.ed25519_pub)
                        .map_or_else(|| sender.user_id(), |proof| proof.name);
                    if !self.certified_sibling(&sender) {
                        let _ = self.incoming.send(ReceivedDm {
                            message_id: envelope.as_ref().map(|env| EventId::new(env.msg_id)),
                            from_account: envelope
                                .as_ref()
                                .map(|env| env.route.sender_account.clone()),
                            from: sender.user_id(),
                            from_name,
                            text: body.text,
                            reply_to: body.reply_to,
                        });
                    }
                }
            }
            if installed?.is_none() {
                break;
            }
        }
        Ok(())
    }

    /// Current profile's local durable acceptance / authenticated delivery state.
    pub fn delivery_status(&self, id: EventId) -> Option<DeliveryStatus> {
        self.delivery
            .lock()
            .expect("delivery lock not poisoned")
            .status(id)
    }

    /// Confirmed chunks across the recipient account's devices. Local staging and
    /// self-device replication are excluded from the sender's upload percentage.
    pub fn outgoing_file_progress(&self, id: EventId) -> Option<super::OutgoingFileProgress> {
        let delivery = self.delivery.lock().expect("delivery lock not poisoned");
        let card = delivery.file_card(id)?;
        let own_account = self.account_id();
        let recipients = card
            .destinations
            .iter()
            .filter(|d| d.binding.account.as_deref() != Some(own_account.as_str()))
            .collect::<Vec<_>>();
        if recipients.is_empty() {
            return None;
        }
        let acks = self
            .outgoing_file_acks
            .lock()
            .expect("outgoing file progress lock not poisoned");
        let consent = if recipients.iter().any(|destination| {
            destination.active
                && self.outgoing_file_consent_status(id, destination.binding.event_id)
                    == Some(crate::eventlog::sync::FileConsentStatus::Declined)
        }) {
            Some(super::ReceiveDecision::Declined)
        } else if recipients.iter().any(|destination| {
            destination.active
                && self.outgoing_file_consent_status(id, destination.binding.event_id)
                    == Some(crate::eventlog::sync::FileConsentStatus::Pending)
        }) {
            Some(super::ReceiveDecision::Pending)
        } else {
            None
        };
        let total = card.chunk_count.saturating_mul(recipients.len() as u32);
        let done = recipients.iter().fold(0u32, |sum, destination| {
            let confirmed = if destination.active {
                acks.get(&(id, destination.binding.event_id))
                    .copied()
                    .unwrap_or(0)
                    .min(card.chunk_count)
            } else {
                card.chunk_count
            };
            sum.saturating_add(confirmed)
        });
        Some(super::OutgoingFileProgress {
            done,
            total,
            delivered: recipients.iter().all(|d| !d.active),
            consent,
        })
    }

    /// Bounded account-conversation status snapshot, with no plaintext payloads.
    pub fn account_delivery_statuses(
        &self,
        peer: &str,
        ids: &[EventId],
    ) -> Vec<(EventId, DeliveryStatus)> {
        // Own-device synchronization has no external recipient delivery claim.
        if peer == self.account_id() {
            return Vec::new();
        }
        let store = self.delivery.lock().expect("delivery lock not poisoned");
        let conv = super::conversation::account_conversation_id(&self.account_id(), peer);
        ids.iter()
            .take(256)
            .filter_map(|id| store.status_in(conv, *id).map(|s| (*id, s)))
            .collect()
    }

    /// A wake hook for hosts; the accept loop also schedules periodic retries.
    pub async fn delivery_work_notified(&self) {
        self.delivery_notify.notified().await;
    }
    pub async fn delivery_status_notified(&self) {
        self.delivery_status_notify.notified().await;
    }

    pub(in crate::node) fn raw_sent_history(
        &self,
        conversation: ConversationId,
    ) -> Vec<(EventId, SentEntry)> {
        let store = self.delivery.lock().expect("delivery lock not poisoned");
        let log = self.log.lock().expect("log lock not poisoned");
        let author = crate::eventlog::Author::from_ed25519(self.identity.public().ed25519_pub);
        let mut ids: std::collections::HashMap<_, _> = log
            .events(&conversation)
            .into_iter()
            .filter(|e| e.kind == crate::eventlog::EventKind::Message && e.author == author)
            .map(|e| (e.seq, e.id))
            .collect();
        let mut entries = self
            .sentlog
            .lock()
            .expect("sent lock not poisoned")
            .entries(&conversation);
        for tx in store.pending_transactions() {
            if let DeliveryTransaction::Outgoing { message, sent, .. } = tx.body()
                && sent.conversation == conversation
            {
                ids.insert(sent.seq, message.logical_id);
                if !entries.iter().any(|e| e == sent) {
                    entries.push(sent.clone());
                }
            }
        }
        let recovered_ids = self
            .recovered_sent_ids
            .lock()
            .expect("recovered sent ids mutex not poisoned");
        entries
            .into_iter()
            .map(|e| {
                (
                    recovered_ids
                        .as_ref()
                        .and_then(|saved| saved.id_for(&e))
                        .or_else(|| ids.get(&e.seq).copied())
                        .unwrap_or(EventId::new([0; 32])),
                    e,
                )
            })
            .collect()
    }

    #[cfg(test)]
    pub(in crate::node) fn sent_entries_for_history(
        &self,
        conversation: ConversationId,
    ) -> Vec<SentEntry> {
        let store = self.delivery.lock().expect("delivery lock not poisoned");
        let mut entries = self
            .sentlog
            .lock()
            .expect("sent lock not poisoned")
            .entries(&conversation);
        for tx in store.pending_transactions() {
            if let DeliveryTransaction::Outgoing { sent, .. } = tx.body()
                && sent.conversation == conversation
                && !entries.iter().any(|e| e == sent)
            {
                entries.push(sent.clone());
            }
        }
        entries
    }

    pub(in crate::node) fn process_delivery_control(
        &self,
        event: &Event,
        proof: &crate::discovery::Announce,
        store: &mut DeliveryStore,
    ) {
        let Some(opened) =
            super::delivery_receipt::open_receipt(&self.identity, &self.account_id(), proof, event)
        else {
            return;
        };
        let canonical = store
            .logical_for_original(opened.original_event_id())
            .unwrap_or_else(|| opened.logical_id());
        let Some(message) = self.immutable_delivery(store, canonical) else {
            return;
        };
        let Some(authenticated) = opened.authenticate(&message) else {
            return;
        };
        // Persist the signed control evidence before exposing the durable status.
        if self
            .log
            .lock()
            .expect("log lock not poisoned")
            .append_durable(event.clone())
            .is_err()
        {
            return;
        }
        if store
            .mark_delivered_for(
                authenticated.logical_id(),
                authenticated.original_event_id(),
            )
            .is_ok()
        {
            self.delivery_observations
                .lock()
                .expect("delivery observations lock not poisoned")
                .retain(|logical, _| logical != authenticated.logical_id());
            self.delivery_status_notify.notify_waiters();
        }
    }

    fn immutable_delivery(&self, store: &DeliveryStore, id: EventId) -> Option<OutgoingDelivery> {
        if let Some(message) = store.message(id) {
            return Some(message.clone());
        }
        let completed = store.completed_work(id)?;
        let log = self.log.lock().expect("log lock not poisoned");
        let mut destinations = Vec::with_capacity(completed.remaining.len());
        for reference in &completed.remaining {
            let event = log.get(&reference.event_id)?;
            if event.author.ed25519_pub() != &self.identity.public().ed25519_pub
                || event.wall_clock != completed.wall_clock
                || event.conversation_id
                    != super::conversation::dm_conversation_id(
                        &self.identity.public(),
                        &reference.device,
                    )
                || !event.verify_integrity()
                || !event.verify_signature()
            {
                return None;
            }
            destinations.push(DeliveryDestination {
                device: reference.device.clone(),
                account: reference.account.clone(),
                event: event.clone(),
                receipt_eligible: reference.receipt_eligible,
            });
        }
        Some(OutgoingDelivery {
            logical_id: id,
            sender_account: self.account_id(),
            recipient_account: completed.recipient_account.clone(),
            conversation: completed.conversation,
            wall_clock: completed.wall_clock,
            destinations,
        })
    }

    /// Immediate best-effort transfer of an already accepted immutable intent.
    /// A transport success is never a delivery confirmation.
    pub async fn flush_delivery(&self, id: EventId) {
        let message = {
            let mut store = self.delivery.lock().expect("delivery lock not poisoned");
            if self.recover_delivery(&mut store).is_err() {
                return;
            }
            self.immutable_delivery(&store, id)
        };
        let Some(message) = message else {
            return;
        };
        for destination in message.destinations {
            if !self.delivery_destination_allowed(&destination) {
                continue;
            }
            let conv = destination.event.conversation_id;
            if let Some(peer) = self
                .routing_peer(&destination.device.user_id())
                .filter(|p| {
                    p.public == destination.device
                        && (destination.account.is_none() || p.account_id == destination.account)
                        && self.known_account_allowed(&p.public)
                })
            {
                self.transfer_delivery_destination(&peer, &destination, conv)
                    .await;
                self.emit_new_messages(conv);
                // The remote serve loop processes plaintext after sync replies;
                // a second round requests control evidence over its own scope.
                let control = super::delivery_receipt::delivery_conversation_id(
                    &self.identity.public(),
                    &peer.public,
                );
                self.transfer_delivery_destination(&peer, &destination, control)
                    .await;
                self.emit_new_messages(control);
            }
            self.replicate_delivery_destination(&destination).await;
        }
    }

    /// Bounded direct receipt transfer hook. The shared worker separately
    /// qualifies exact durable custody and retires queued work.
    pub async fn flush_delivery_receipts(&self, limit: usize) {
        let receipts = {
            let mut store = self.delivery.lock().expect("delivery lock not poisoned");
            if self.recover_delivery(&mut store).is_err() {
                return;
            }
            store.retry_receipts(limit)
        };
        for receipt in receipts {
            let destination = receipt.destination;
            if !self.delivery_destination_allowed(&destination) {
                continue;
            }
            if let Some(peer) = self
                .routing_peer(&destination.device.user_id())
                .filter(|p| {
                    p.public == destination.device
                        && (destination.account.is_none() || p.account_id == destination.account)
                        && self.known_account_allowed(&p.public)
                })
            {
                self.transfer_delivery_destination(
                    &peer,
                    &destination,
                    destination.event.conversation_id,
                )
                .await;
                self.emit_new_messages(destination.event.conversation_id);
            }
        }
    }

    pub(in crate::node) async fn transfer_delivery_destination(
        &self,
        peer: &crate::discovery::PeerRecord,
        destination: &DeliveryDestination,
        conversation: ConversationId,
    ) -> bool {
        for fresh in [false, true] {
            let started = std::time::Instant::now();
            let dial = if fresh {
                self.privacy_dial(peer.addr, &destination.device)
                    .await
                    .map(|channel| (channel, false))
            } else {
                self.privacy_sync_dial_with_reuse(peer.addr, &destination.device)
                    .await
            };
            let Ok((mut channel, reused)) = dial else {
                return false;
            };
            if !self.delivery_destination_allowed(destination)
                || channel.peer_announcement().is_some_and(|p| {
                    destination.account.is_some() && p.account_id() != destination.account
                })
                || !self
                    .delivery
                    .lock()
                    .expect("delivery lock not poisoned")
                    .contains_work_event(destination.event.id)
            {
                return false;
            }
            let exchange = async {
                let store = self.sync_store(channel.peer_identity());
                super::session::request_round(&mut channel, &store, conversation).await?;
                if conversation != destination.event.conversation_id {
                    return Ok(false);
                }
                super::session::request_durable_have(
                    &mut channel,
                    conversation,
                    destination.event.id,
                )
                .await
            }
            .await;
            match exchange {
                Ok(held) => {
                    self.observe_control_exchange(&channel, started);
                    if held || conversation != destination.event.conversation_id {
                        self.recycle_sync_channel(channel);
                    }
                    return held && self.delivery_destination_allowed(destination);
                }
                Err(error) if reused && super::session_pool::retryable_sync_error(&error) => {}
                Err(_) => return false,
            }
        }
        false
    }

    pub(in crate::node) async fn replicate_delivery_destination(
        &self,
        destination: &DeliveryDestination,
    ) -> bool {
        // A validated receipt retires this exact destination. Avoid opening a
        // new relay session for stale snapshots; other device copies remain
        // independent work. Recheck after dialing below for concurrent receipts.
        if !self.delivery_destination_allowed(destination)
            || !self
                .delivery
                .lock()
                .expect("delivery lock not poisoned")
                .contains_work_event(destination.event.id)
        {
            return false;
        }
        let candidates = {
            let roster = self.roster.lock().expect("roster lock not poisoned");
            self.post_office_candidates_for(&roster, &destination.device)
        };
        use futures_util::StreamExt;
        let scope = format!("upload:{}", destination.device.user_id());
        let candidates = self.relay_candidate_batch(&scope, candidates);
        let attempts = futures_util::stream::iter(candidates)
            .map(|po| async move {
                if po.public == self.identity.public()
                    || !self.relay_send_allowed(&po.public)
                    || !self.relay_retry_due(&po.public)
                {
                    return false;
                }
                let success = self
                    .adaptive_control_timeout(&po.public, async {
                        for fresh in [false, true] {
                            let started = std::time::Instant::now();
                            let dial = if fresh {
                                self.privacy_dial(po.addr, &po.public)
                                    .await
                                    .map(|channel| (channel, false))
                            } else {
                                self.privacy_sync_dial_with_reuse(po.addr, &po.public).await
                            };
                            let Ok((mut channel, reused)) = dial else {
                                return false;
                            };
                            if !self.delivery_destination_allowed(destination)
                                || !self.relay_send_allowed(&po.public)
                                || channel.peer_announcement().is_some_and(|p| {
                                    po.account_id.is_some() && p.account_id() != po.account_id
                                })
                                || !self
                                    .delivery
                                    .lock()
                                    .expect("delivery lock not poisoned")
                                    .contains_work_event(destination.event.id)
                            {
                                return false;
                            }
                            let exchange = async {
                                let uploaded = self
                                    .opaque_relay_round(
                                        &mut channel,
                                        &destination.device,
                                        Some(destination.event.conversation_id),
                                    )
                                    .await?;
                                let Some((_, outer_id)) = uploaded
                                    .iter()
                                    .find(|(inner_id, _)| *inner_id == destination.event.id)
                                else {
                                    return Ok(false);
                                };
                                let Ok(mailbox) = crate::postoffice::opaque::RelayMailbox::new(
                                    &self.identity,
                                    &destination.device,
                                    &po.public,
                                ) else {
                                    return Ok(false);
                                };
                                super::session::request_durable_have(
                                    &mut channel,
                                    mailbox.id(),
                                    *outer_id,
                                )
                                .await
                            }
                            .await;
                            match exchange {
                                Ok(held) => {
                                    self.observe_control_exchange(&channel, started);
                                    if held {
                                        self.recycle_sync_channel(channel);
                                    }
                                    return held
                                        && self.delivery_destination_allowed(destination)
                                        && self.relay_send_allowed(&po.public);
                                }
                                Err(error)
                                    if reused
                                        && super::session_pool::retryable_sync_error(&error) => {}
                                Err(_) => return false,
                            }
                        }
                        false
                    })
                    .await
                    .unwrap_or(false);
                self.record_relay_attempt(&po.public, success);
                success
            })
            .buffer_unordered(2);
        tokio::pin!(attempts);
        while let Some(held) = attempts.next().await {
            if held {
                return true;
            }
        }
        false
    }

    /// A failed bounded batch continues at the next candidates on a later
    /// wake. Prefix-scoped keys separate upload, pull and relay-drain turns.
    pub(in crate::node) fn relay_candidate_batch(
        &self,
        scope: &str,
        mut candidates: Vec<crate::discovery::roster::PeerRecord>,
    ) -> Vec<crate::discovery::roster::PeerRecord> {
        if candidates.is_empty() {
            return candidates;
        }
        let mut cursors = self
            .relay_drain_cursor
            .lock()
            .expect("relay cursor lock not poisoned");
        let offset = cursors.get(scope).copied().unwrap_or(0) % candidates.len();
        candidates.rotate_left(offset);
        let next = offset.wrapping_add(candidates.len().min(4));
        if cursors.len() >= MAX_RELAY_CUSTODY_HINTS
            && !cursors.contains_key(scope)
            && let Some(key) = cursors.keys().next().cloned()
        {
            cursors.remove(&key);
        }
        cursors.insert(scope.to_owned(), next);
        candidates.truncate(4);
        candidates
    }

    pub(in crate::node) fn relay_retry_due(&self, peer: &PublicIdentity) -> bool {
        self.relay_retry_after
            .lock()
            .expect("relay retry lock not poisoned")
            .get(&peer.user_id())
            .is_none_or(|until| *until <= std::time::Instant::now())
    }

    pub(in crate::node) fn record_relay_attempt(&self, peer: &PublicIdentity, success: bool) {
        let mut hints = self
            .relay_retry_after
            .lock()
            .expect("relay retry lock not poisoned");
        if success {
            hints.remove(&peer.user_id());
        } else {
            hints.retain(|_, until| *until > std::time::Instant::now());
            if hints.len() >= MAX_RELAY_CUSTODY_HINTS
                && let Some(key) = hints.keys().next().cloned()
            {
                hints.remove(&key);
            }
            hints.insert(
                peer.user_id(),
                std::time::Instant::now() + RELAY_FAILURE_COOLDOWN,
            );
        }
    }

    fn control_routes(&self, peer: &PublicIdentity) -> ControlRoutes {
        let roster = self.roster.lock().expect("roster mutex not poisoned");
        let mut endpoints = std::collections::BTreeSet::new();
        let mut publicly_observed = false;
        if let Some(record) = roster
            .get(&peer.user_id())
            .filter(|record| record.public == *peer)
        {
            publicly_observed = !record
                .presence_evidence(std::time::Instant::now())
                .is_empty();
            endpoints.extend(
                record
                    .routes
                    .iter()
                    .filter(|route| {
                        route.source != crate::discovery::roster::RouteSource::PostOffice
                    })
                    .map(|route| (peer.user_id(), route.addr)),
            );
        }
        if self
            .post_office_enabled
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            for relay in self.post_office_candidates_for(&roster, peer) {
                endpoints.insert((relay.public.user_id(), relay.addr));
            }
        }
        ControlRoutes {
            peer: peer.clone(),
            endpoints,
            publicly_observed,
        }
    }

    fn control_budget(&self, peer: &PublicIdentity) -> std::time::Duration {
        self.routing_peer(&peer.user_id())
            .filter(|p| p.public == *peer)
            .map_or(CONTROL_EXCHANGE_TIMEOUT, |record| {
                record
                    .routes
                    .iter()
                    .filter(|route| {
                        route.source != crate::discovery::roster::RouteSource::PostOffice
                    })
                    .map(|route| self.sessions.control_timeout(peer, route.addr))
                    .chain(std::iter::once(
                        self.sessions.control_timeout(peer, record.addr),
                    ))
                    .max()
                    .unwrap_or(CONTROL_EXCHANGE_TIMEOUT)
            })
    }

    pub(super) async fn adaptive_control_timeout<T>(
        &self,
        peer: &PublicIdentity,
        operation: impl std::future::Future<Output = T>,
    ) -> Option<T> {
        let started = tokio::time::Instant::now();
        let mut budget = self.control_budget(peer);
        tokio::pin!(operation);
        loop {
            tokio::select! {
                result = &mut operation => return Some(result),
                _ = tokio::time::sleep_until(started + budget) => {
                    // Authentication completed during this exchange may have
                    // taught us that its remaining round trips need more time.
                    budget = budget.max(self.control_budget(peer));
                    if tokio::time::Instant::now() < started + budget { continue; }
                    let preferred = self.preferred_routes.lock().expect("routes lock not poisoned").get(&peer.user_id()).copied();
                    let address = preferred.or_else(|| self.routing_peer(&peer.user_id()).filter(|p| p.public == *peer).map(|p| p.addr));
                    if let Some(address) = address { self.sessions.control_timed_out(peer, address); }
                    return None;
                }
            }
        }
    }

    pub(super) fn observe_control_exchange(
        &self,
        channel: &crate::transport::SecureChannel<tokio::net::TcpStream>,
        started: std::time::Instant,
    ) {
        if let Ok(address) = channel.tcp_peer_addr() {
            self.sessions
                .control_succeeded(channel.peer_identity(), address, started.elapsed());
        }
    }

    /// Shared by desktop, CLI and SDK accept-loop hosts. The future owns all
    /// cursors and network work, so dropping it cancels scheduling and dials.
    pub(in crate::node) async fn run_delivery_loop(self: std::sync::Arc<Self>) {
        use std::time::Duration;
        let _observation_scope = ObservationScope(self.clone());
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut destination_cursor = None;
        let mut receipt_cursor = None;
        let mut peer_cursor = None;
        let mut file_scheduler = FileDeliveryScheduler::default();
        let mut diagnostic = DeliveryDiagnosticThrottle::default();
        // JoinSet aborts its tasks on drop: retiring the worker also retires
        // every control dial/exchange. Never detach work from this lifetime.
        let mut controls = tokio::task::JoinSet::new();
        let mut active: std::collections::HashMap<ControlWork, ControlRoutes> =
            std::collections::HashMap::new();
        let mut retries = super::delivery_retry::RetrySchedule::default();
        let mut network = self.network_changes.subscribe();
        loop {
            tokio::select! {
                result = file_scheduler.workers.join_next(), if !file_scheduler.workers.is_empty() => {
                    if let Some(result) = result {
                        if let Err(error) = &result { log::error!("automatic file delivery task failed: {error}"); return; }
                        file_scheduler.finish(&self, result, &mut diagnostic);
                    }
                    continue;
                },
                _ = interval.tick() => {},
                _ = self.delivery_notify.notified() => {},
                _ = network.changed() => {
                    self.clear_delivery_observations();
                    self.sessions.network_changed();
                    file_scheduler.workers.shutdown().await;
                    file_scheduler.running_sockets.clear();
                    file_scheduler.transfers.clear();
                    file_scheduler.relay_cooldowns.clear();
                    self.relay_retry_after.lock().expect("relay retry lock not poisoned").clear();
                    controls.shutdown().await;
                    self.clear_delivery_observations();
                    active.clear();
                    retries.clear();
                    self.channel_recovery_notify.notify_one();
                    self.account_sync_notify.notify_one();
                    self.bridge_notify.notify_one();
                    continue;
                },
                result = controls.join_next(), if !controls.is_empty() => {
                    match result {
                        Some(Ok((key, persistence_failed, progress, observation))) => {
                            if let Some(routes) = active.remove(&key) {
                                retries.finished(key.retry_key(&routes.peer), routes.retry_routes(), progress, std::time::Instant::now());
                                if let Some((token, reason)) = observation { self.finish_delivery_observation_with_reason(token, reason); }
                            }
                            if persistence_failed {
                                diagnostic.warn_if_due();
                            }
                        }
                        // A panicked child cannot leave a permanently occupied
                        // lane. Retire this worker and cancel its remaining work.
                        Some(Err(error)) => {
                            log::error!("automatic delivery control task failed: {error}");
                            return;
                        }
                        None => {}
                    }
                    continue;
                },
            }
            // A new route for an active peer/eligible relay must not wait behind
            // an exchange still blocked on its old endpoint. Refresh timestamps
            // and unrelated peers do not invalidate the bounded control batch.
            if active
                .values()
                .any(|routes| self.control_routes(&routes.peer).endpoints != routes.endpoints)
            {
                self.clear_delivery_observations();
                controls.shutdown().await;
                self.clear_delivery_observations();
                active.clear();
            }
            self.prune_delivery_observations();
            // Reading next_destination advances its per-message cursor. Only
            // select work when its lane can admit it, so busy lanes preserve
            // the next queued turn instead of repeatedly skipping it.
            let destination_capacity = active
                .keys()
                .filter(|key| matches!(key, ControlWork::Destination(_)))
                .count()
                < 2;
            let receipt_capacity = !active
                .keys()
                .any(|key| matches!(key, ControlWork::Receipt(_)));
            let file_scan = file_scheduler.scan(&self);
            let node = self.clone();
            let Some(work) = self.runtime_work.admit() else {
                return;
            };
            let snapshot = tokio::task::spawn_blocking(move || {
                let _work = work;
                #[cfg(test)]
                if let Some(hook) = node.delivery_snapshot_hook.lock().unwrap().take() {
                    hook();
                }
                let mut store = node.delivery.lock().expect("delivery lock not poisoned");
                if node.recover_delivery(&mut store).is_err() {
                    return None;
                }
                let next = destination_capacity
                    .then(|| store.next_destination(destination_cursor))
                    .flatten();
                let destination = next.and_then(|(id, reference)| {
                    let event = node
                        .log
                        .lock()
                        .expect("log lock not poisoned")
                        .get(&reference.event_id)?
                        .clone();
                    Some((
                        id,
                        DeliveryDestination {
                            device: reference.device,
                            account: reference.account,
                            event,
                            receipt_eligible: reference.receipt_eligible,
                        },
                    ))
                });
                let receipt = if receipt_capacity {
                    store
                        .retry_receipts_after(receipt_cursor, 1)
                        .into_iter()
                        .next()
                        .or_else(|| store.retry_receipts_after(None, 1).into_iter().next())
                } else {
                    None
                };
                let (files, cursor) = file_scan.read(&store);
                Some((destination, receipt, files, cursor))
            })
            .await;
            let Ok(Some((destination, receipt, files, cursor))) = snapshot else {
                diagnostic.warn_if_due();
                continue;
            };
            if let Some((id, destination)) = destination {
                destination_cursor = Some(id);
                let key = ControlWork::Destination(destination.event.id);
                let routes = self.control_routes(&destination.device);
                if active
                    .keys()
                    .filter(|key| matches!(key, ControlWork::Destination(_)))
                    .count()
                    < 2
                    && !active.contains_key(&key)
                    && !active.iter().any(|(key, routes)| {
                        matches!(key, ControlWork::Destination(_))
                            && routes.peer == destination.device
                    })
                    && retries.ready(
                        &key.retry_key(&destination.device),
                        &routes.retry_routes(),
                        std::time::Instant::now(),
                    )
                {
                    let Some(work) = self.runtime_work.admit() else {
                        return;
                    };
                    let node = self.clone();
                    // Capture the exact admitted attempt before spawning. A
                    // cancelled task may never be polled; it still owns cleanup,
                    // and cannot publish a new hint after its parent is dropped.
                    let attempt = ObservationAttempt {
                        node: node.clone(),
                        token: node.begin_delivery_observation(id, &destination),
                    };
                    active.insert(key.clone(), routes);
                    controls.spawn(work.track(async move {
                        // Move the entire Drop owner into the future. Capturing
                        // only its Copy token would drop the guard at admission.
                        let mut attempt = attempt;
                        let live = node
                            .delivery
                            .lock()
                            .expect("delivery lock not poisoned")
                            .contains_destination(id, destination.event.id);
                        let (progress, reason) = if live {
                            node.retry_one_destination_with_reason(&destination).await
                        } else {
                            (false, None)
                        };
                        let failed = if progress {
                            let mut store =
                                node.delivery.lock().expect("delivery lock not poisoned");
                            store.completed_work(id).is_some()
                                && store.retire_destination(id, destination.event.id).is_err()
                        } else {
                            false
                        };
                        let observation = attempt.token.take().map(|token| (token, reason));
                        (key, failed, progress, observation)
                    }));
                }
            }
            if let Some(receipt) = receipt {
                receipt_cursor = Some(receipt.destination.event.id);
                let key = ControlWork::Receipt(receipt.destination.event.id);
                let routes = self.control_routes(&receipt.destination.device);
                if !active
                    .keys()
                    .any(|key| matches!(key, ControlWork::Receipt(_)))
                    && !active.contains_key(&key)
                    && retries.ready(
                        &key.retry_key(&receipt.destination.device),
                        &routes.retry_routes(),
                        std::time::Instant::now(),
                    )
                {
                    let Some(work) = self.runtime_work.admit() else {
                        return;
                    };
                    let node = self.clone();
                    active.insert(key.clone(), routes);
                    controls.spawn(work.track(async move {
                        let progress = node.retry_one_destination(&receipt.destination).await;
                        let failed = progress
                            && node
                                .delivery
                                .lock()
                                .expect("delivery lock not poisoned")
                                .finish_receipt(receipt.conversation, receipt.original_event_id)
                                .is_err();
                        (key, failed, progress, None)
                    }));
                }
            }
            file_scheduler.admit(&self, files, cursor);
            file_scheduler.advance(&self);
            // Historical identity authorizes a control pull even with its sender
            // offline. The cursor advances before any bounded network await.
            let peer = if active.keys().any(|key| matches!(key, ControlWork::Pull(_))) {
                None
            } else {
                let state = self
                    .privacy
                    .state
                    .read()
                    .expect("privacy lock not poisoned");
                if let Some(state) = state.as_ref() {
                    state.proofs.next_after(peer_cursor.as_deref())
                } else {
                    self.roster
                        .lock()
                        .expect("roster lock not poisoned")
                        .next_historical_after(peer_cursor.as_deref())
                }
            };
            if let Some(peer) = peer {
                peer_cursor = Some(peer.user_id.clone());
                if peer.post_office
                    || peer.public() == self.identity.public()
                    || !self.known_account_allowed(&peer.public())
                    || !self.verified_dm_pull_peer(&peer.user_id)
                {
                    continue;
                }
                let key = ControlWork::Pull(peer.user_id.clone());
                let routes = self.control_routes(&peer.public());
                if !active.keys().any(|key| matches!(key, ControlWork::Pull(_)))
                    && !active.contains_key(&key)
                    && retries.ready(
                        &key.retry_key(&peer.public()),
                        &routes.retry_routes(),
                        std::time::Instant::now(),
                    )
                {
                    let Some(work) = self.runtime_work.admit() else {
                        return;
                    };
                    let node = self.clone();
                    active.insert(key.clone(), routes);
                    controls.spawn(work.track(async move {
                        let progress = node.reconcile_delivery_peer(&peer).await;
                        (key, false, progress, None)
                    }));
                }
            }
        }
    }

    async fn reconcile_delivery_peer(&self, peer: &crate::discovery::Announce) -> bool {
        if !self.verified_dm_pull_peer(&peer.user_id) {
            return false;
        }
        let conv = super::delivery_receipt::delivery_conversation_id(
            &self.identity.public(),
            &peer.public(),
        );
        let dm = super::conversation::dm_conversation_id(&self.identity.public(), &peer.public());
        tokio::time::timeout(super::route_timing::MAX_CONTROL_DEADLINE, async {
            let (direct, relay) = tokio::join!(
                async {
                    let messages = Box::pin(self.pull_delivery_scope(peer, dm, false)).await;
                    let receipts = Box::pin(self.pull_delivery_scope(peer, conv, false)).await;
                    messages || receipts
                },
                async {
                    let messages = Box::pin(self.pull_delivery_scope(peer, dm, true)).await;
                    let receipts = Box::pin(self.pull_delivery_scope(peer, conv, true)).await;
                    messages || receipts
                }
            );
            direct || relay
        })
        .await
        .unwrap_or(false)
    }

    async fn pull_delivery_scope(
        &self,
        proof: &crate::discovery::Announce,
        conv: ConversationId,
        relay: bool,
    ) -> bool {
        let current = || {
            self.verified_dm_pull_peer(&proof.user_id)
                && self.historical_author(&proof.ed25519_pub).is_some_and(|p| {
                    p.public() == proof.public() && p.account_id() == proof.account_id()
                })
                && !self
                    .delivery_suspended
                    .load(std::sync::atomic::Ordering::Acquire)
        };
        if !current() {
            return false;
        }
        let peers = if relay {
            let roster = self.roster.lock().expect("roster lock not poisoned");
            self.post_office_candidates(&roster)
        } else {
            self.routing_peer(&proof.public().user_id())
                .into_iter()
                .collect()
        };
        let peers = if relay {
            self.relay_candidate_batch(&format!("pull:{}", proof.user_id), peers)
        } else {
            peers
        };
        let mut completed = false;
        for peer in peers {
            let started = std::time::Instant::now();
            if relay && !self.relay_receive_allowed(&peer.public) {
                continue;
            }
            if !relay && (peer.public != proof.public() || peer.account_id != proof.account_id()) {
                continue;
            }
            if relay && !self.relay_retry_due(&peer.public) {
                continue;
            }
            let success = self
                .adaptive_control_timeout(&peer.public, async {
                    let Ok(mut channel) = self.privacy_sync_dial(peer.addr, &peer.public).await
                    else {
                        return false;
                    };
                    if !current()
                        || (relay && !self.relay_receive_allowed(&peer.public))
                        || channel
                            .peer_announcement()
                            .is_some_and(|p| p.account_id() != peer.account_id)
                    {
                        return false;
                    }
                    let success = if relay {
                        self.opaque_relay_round(&mut channel, &proof.public(), None)
                            .await
                            .is_ok()
                    } else {
                        let store = self.pull_sync_store(channel.peer_identity());
                        super::session::request_round(&mut channel, &store, conv)
                            .await
                            .is_ok()
                    };
                    if success {
                        self.observe_control_exchange(&channel, started);
                        self.recycle_sync_channel(channel);
                        self.emit_new_messages(conv);
                        self.process_file_events(conv);
                    }
                    success
                })
                .await
                .unwrap_or(false);
            if relay {
                self.record_relay_attempt(&peer.public, success);
            }
            completed |= success;
        }
        completed
    }

    async fn retry_one_destination(&self, destination: &DeliveryDestination) -> bool {
        self.retry_one_destination_with_reason(destination).await.0
    }

    async fn retry_one_destination_with_reason(
        &self,
        destination: &DeliveryDestination,
    ) -> (bool, Option<DeliveryRecoveryReason>) {
        if !self.delivery_destination_allowed(destination) {
            return (false, None);
        }
        let direct_peer = self
            .routing_peer(&destination.device.user_id())
            .filter(|p| {
                p.public == destination.device
                    && p.account_id == destination.account
                    && self.known_account_allowed(&p.public)
            });
        // Snapshot only locally known and permitted alternatives. No remote
        // presence conclusion follows from an empty eligible route set.
        let reason = if direct_peer.is_none() {
            if !self
                .post_office_enabled
                .load(std::sync::atomic::Ordering::Acquire)
                || !self
                    .post_office_send_enabled
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                Some(DeliveryRecoveryReason::PostOfficeDisabled)
            } else {
                let candidates = {
                    let roster = self.roster.lock().expect("roster lock not poisoned");
                    self.post_office_candidates_for(&roster, &destination.device)
                };
                (!candidates.iter().any(|p| {
                    p.public != self.identity.public() && self.relay_send_allowed(&p.public)
                }))
                .then_some(DeliveryRecoveryReason::NoKnownRoute)
            }
        } else {
            None
        };
        let started = std::time::Instant::now();
        let budget = super::route_timing::MAX_CONTROL_DEADLINE;
        let direct = async {
            let mut held = false;
            let mut timed_out = false;
            if let Some(peer) = direct_peer {
                let outcome = self
                    .adaptive_control_timeout(&peer.public, async {
                        let held = self
                            .transfer_delivery_destination(
                                &peer,
                                destination,
                                destination.event.conversation_id,
                            )
                            .await;
                        self.emit_new_messages(destination.event.conversation_id);
                        if held {
                            let control = super::delivery_receipt::delivery_conversation_id(
                                &self.identity.public(),
                                &peer.public,
                            );
                            self.transfer_delivery_destination(&peer, destination, control)
                                .await;
                            self.emit_new_messages(control);
                        }
                        held
                    })
                    .await;
                timed_out = outcome.is_none();
                held = outcome.unwrap_or(false);
            }
            (held, started.elapsed(), timed_out)
        };
        let relay = async {
            let outcome =
                tokio::time::timeout(budget, self.replicate_delivery_destination(destination))
                    .await;
            let timed_out = outcome.is_err();
            (outcome.unwrap_or(false), started.elapsed(), timed_out)
        };
        tokio::pin!(direct, relay);
        // Both alternatives share the same bounded wall-clock window. Custody
        // cannot cancel recipient progress; exact recipient confirmation can.
        let ((held, direct_elapsed, direct_timeout), (relay_held, relay_elapsed, relay_timeout)) = tokio::select! {
            result = &mut direct => {
                let still_pending = self.delivery.lock().expect("delivery lock not poisoned")
                    .contains_work_event(destination.event.id);
                if still_pending { (result, relay.await) }
                else { (result, (false, started.elapsed(), false)) }
            },
            result = &mut relay => (direct.await, result),
        };
        if started.elapsed() >= std::time::Duration::from_secs(10) {
            log::warn!(target: "mesh_talk::delivery",
                "slow delivery attempt direct_ms={} relay_ms={} direct_held={} relay_held={} budget_ms={}",
                direct_elapsed.as_millis(), relay_elapsed.as_millis(), held, relay_held, budget.as_millis());
        }
        let progress = held || relay_held;
        (
            progress,
            if progress {
                None
            } else if direct_timeout || relay_timeout {
                Some(DeliveryRecoveryReason::AttemptTimedOut)
            } else {
                reason
            },
        )
    }

    pub(in crate::node) fn install_file_card_scopes(&self, store: &DeliveryStore) {
        self.install_file_card_scopes_for(store, None);
    }

    fn install_file_card_scopes_for(&self, store: &DeliveryStore, current: Option<EventId>) {
        let cards = if let Some(id) = current {
            store.file_card(id).into_iter().collect::<Vec<_>>()
        } else {
            store.file_cards().collect::<Vec<_>>()
        };
        let scopes = {
            let rows = self
                .received_files
                .lock()
                .expect("file rows lock not poisoned");
            let log = self.log.lock().expect("log lock not poisoned");
            cards
                .into_iter()
                .filter(|file| {
                    rows.entry(file.id).is_some_and(|entry| {
                        entry.conversation == file.conversation
                            && entry.from == self.user_id()
                            && super::files::eligible_manifest_row(
                                entry,
                                log.get(&entry.event_id),
                                None,
                            )
                            .is_some_and(|manifest| {
                                manifest.file_conv() == file.file_conversation
                                    && manifest.chunk_count() == file.chunk_count
                            })
                    })
                })
                .flat_map(|file| {
                    file.destinations.iter().filter_map(|destination| {
                        let event = log.get(&destination.binding.event_id)?;
                        (event.kind == crate::eventlog::EventKind::FileManifest
                            && event.author.ed25519_pub() == &self.identity.public().ed25519_pub
                            && event.conversation_id
                                == super::conversation::dm_conversation_id(
                                    &self.identity.public(),
                                    &destination.binding.device,
                                )
                            && event.verify_signature()
                            && event.verify_integrity())
                        .then_some((
                            file.file_conversation,
                            event.conversation_id,
                            event.author,
                            event.id,
                        ))
                    })
                })
                .collect::<Vec<_>>()
        };
        for (file, parent, author, event) in scopes {
            self.remember_manifest_scope(file, parent, author, event);
        }
    }

    async fn retry_file_destination(
        &self,
        file: &super::delivery_store::FileCard,
        destination: &super::delivery_store::FileDestination,
    ) -> bool {
        self.retry_file_destination_inner(file, destination, true)
            .await
    }

    fn file_destination_current(
        &self,
        file: &super::delivery_store::FileCard,
        destination: &super::delivery_store::FileDestination,
    ) -> bool {
        let binding = &destination.binding;
        self.delivery
            .lock()
            .expect("delivery lock not poisoned")
            .contains_file_destination(file.id, binding.event_id, &binding.device, &binding.account)
            && !self
                .delivery_suspended
                .load(std::sync::atomic::Ordering::Acquire)
            && self
                .historical_author(&binding.device.ed25519_pub)
                .is_some_and(|proof| {
                    proof.public() == binding.device && proof.account_id() == binding.account
                })
            && self.known_account_allowed(&binding.device)
    }

    pub(in crate::node) async fn retry_file_step(
        &self,
        file: &super::delivery_store::FileCard,
        destination: &super::delivery_store::FileDestination,
        transfers: &mut std::collections::HashMap<(EventId, EventId), FileTransfer>,
    ) -> bool {
        self.prune_file_transfers(transfers);
        let key = (file.id, destination.binding.event_id);
        if !self.file_destination_current(file, destination) {
            transfers.remove(&key);
            return false;
        }
        if self.recipient_completed_file(file, destination) {
            transfers.remove(&key);
            return true;
        }
        if !transfers.contains_key(&key) {
            if transfers.len() == MAX_FILE_TRANSFERS {
                // The worker admits only available slots; standalone callers
                // leave excess work durable for the next scheduled turn.
                return false;
            }
            transfers.insert(key, FileTransfer::new(file.clone(), destination.clone()));
        }
        #[cfg(test)]
        self.file_transfer_peak
            .fetch_max(transfers.len(), std::sync::atomic::Ordering::SeqCst);
        // Retained file sockets share the node's outbound admission pool with
        // receipts, privacy refresh, and mailbox reconciliation. Leave one
        // directional slot available instead of timing out a Dial every pass.
        if transfers
            .get(&key)
            .is_some_and(|transfer| matches!(transfer.phase, FilePhase::Dial))
            && transfers
                .values()
                .filter(|transfer| transfer.channel.is_some())
                .count()
                >= MAX_FILE_SOCKETS
        {
            return false;
        }
        let transfer = transfers.get_mut(&key).unwrap();
        // Standalone callers may resume a clean consent boundary directly;
        // the shared scheduler normally parks it outside active capacity.
        if matches!(transfer.phase, FilePhase::ConsentWait) {
            transfer.phase = FilePhase::Dial;
        }
        transfer.last_used = std::time::Instant::now();
        #[cfg(test)]
        let mut timing = TestTiming::new(match transfer.phase {
            FilePhase::Dial => "file-dial",
            FilePhase::Manifest => "file-manifest-exchange",
            FilePhase::ManifestCustody => "file-manifest-custody",
            FilePhase::Consent => "file-consent",
            FilePhase::ConsentWait => "file-consent-wait",
            FilePhase::InitialCustody => "file-initial-custody",
            FilePhase::Chunks => "file-chunk-exchange",
            FilePhase::FinalCustody => "file-final-custody",
            FilePhase::RelayManifestLegacy => "file-relay-legacy-manifest",
            FilePhase::RelayManifestFederated => "file-relay-federated-manifest",
            FilePhase::RelayChunksLegacy => "file-relay-legacy-chunks",
            FilePhase::RelayChunksFederated => "file-relay-federated-chunks",
            FilePhase::RelayCustody => "file-relay-custody",
            FilePhase::RelayHeld => "file-relay-held",
        });
        let outcome =
            tokio::time::timeout(FILE_EXCHANGE_TIMEOUT, self.advance_file_transfer(transfer)).await;
        #[cfg(test)]
        let phase = transfer.phase;
        #[cfg(test)]
        {
            timing.mark(match &outcome {
                Ok(Ok(true)) => "file-retirable",
                Ok(Ok(false)) => "file-clean-boundary",
                Ok(Err(_)) => "file-error",
                Err(_) => "file-exchange-cancelled",
            });
            timing.finish();
        }
        match outcome {
            Ok(Ok(true)) => {
                if let Some(mut transfer) = transfers.remove(&key)
                    && matches!(
                        transfer.phase,
                        FilePhase::InitialCustody | FilePhase::FinalCustody
                    )
                    && transfer.route.as_ref().is_some_and(|(relay, _)| !relay)
                    && let Some(channel) = transfer.channel.take()
                {
                    self.recycle_sync_channel(channel);
                }
                true
            }
            Ok(Ok(false)) => false,
            _ => {
                #[cfg(test)]
                eprintln!(
                    "file-step failure stage={phase:?} timeout={}",
                    outcome.is_err()
                );
                // A cancelled read/write may have consumed part of a frame.
                // Close the socket and restart from the remote's durable Have.
                let transfer = transfers.get_mut(&key).unwrap();
                transfer.channel = None;
                transfer.sync = super::session::SyncProgress::default();
                transfer.opaque = None;
                transfer.route = None;
                transfer.relay_custody = None;
                transfer.phase = FilePhase::Dial;
                transfer.candidate_cursor += 1;
                if transfer.candidate_cursor >= transfer.candidate_count {
                    transfers.remove(&key);
                }
                false
            }
        }
    }

    fn prune_file_transfers(
        &self,
        transfers: &mut std::collections::HashMap<(EventId, EventId), FileTransfer>,
    ) {
        transfers.retain(|_, transfer| {
            transfer.last_used.elapsed() < FILE_TRANSFER_IDLE
                && self.file_destination_current(&transfer.file, &transfer.destination)
                && transfer
                    .channel
                    .as_ref()
                    .is_none_or(|channel| channel.io_admitted())
        });
    }

    fn prune_outgoing_file_acks(&self) {
        let delivery = self.delivery.lock().expect("delivery lock not poisoned");
        self.outgoing_file_acks
            .lock()
            .expect("outgoing file progress lock not poisoned")
            .retain(|(id, destination_id), _| {
                delivery.file_card(*id).is_some_and(|card| {
                    card.destinations
                        .iter()
                        .any(|d| d.active && d.binding.event_id == *destination_id)
                })
            });
        self.outgoing_file_consents
            .lock()
            .expect("outgoing consent lock not poisoned")
            .retain(|(id, destination_id), _| {
                delivery.file_card(*id).is_some_and(|card| {
                    card.destinations
                        .iter()
                        .any(|d| d.active && d.binding.event_id == *destination_id)
                })
            });
    }

    async fn advance_file_transfer(
        &self,
        transfer: &mut FileTransfer,
    ) -> Result<bool, SessionError> {
        let file = &transfer.file;
        let destination = &transfer.destination;
        let binding = &destination.binding;
        if !self.file_destination_current(file, destination) {
            return Err(SessionError::UnexpectedMessage);
        }
        if matches!(transfer.phase, FilePhase::Dial) {
            let mut candidates = Vec::new();
            if let Some(peer) = self
                .routing_peer(&binding.device.user_id())
                .filter(|peer| peer.public == binding.device && peer.account_id == binding.account)
            {
                candidates.push((false, peer));
            }
            if self.outgoing_file_consent_status(file.id, binding.event_id)
                == Some(crate::eventlog::sync::FileConsentStatus::Accepted)
            {
                let relays = {
                    let roster = self.roster.lock().expect("roster lock not poisoned");
                    self.post_office_candidates_for(&roster, &binding.device)
                };
                candidates.extend(
                    relays
                        .into_iter()
                        .filter(|peer| self.relay_send_allowed(&peer.public))
                        .map(|peer| (true, peer)),
                );
            }
            if candidates.is_empty() {
                return Err(SessionError::UnexpectedMessage);
            }
            transfer.candidate_count = candidates.len();
            let index = transfer.candidate_cursor;
            if index >= candidates.len() {
                return Err(SessionError::NoProgress);
            }
            let (relay, peer) = candidates.swap_remove(index);
            let channel = self.privacy_sync_dial(peer.addr, &peer.public).await?;
            if !self.file_destination_current(file, destination)
                || channel.peer_identity() != &peer.public
                || channel
                    .peer_announcement()
                    .is_some_and(|proof| proof.account_id() != peer.account_id)
            {
                return Err(SessionError::UnexpectedMessage);
            }
            transfer.route = Some((relay, peer));
            transfer.channel = Some(channel);
            transfer.phase = if relay {
                FilePhase::RelayManifestLegacy
            } else {
                FilePhase::Manifest
            };
            return Ok(false);
        }
        if transfer.route.as_ref().is_some_and(|(relay, _)| *relay) {
            return self.advance_relay_file_transfer(transfer).await;
        }
        let channel = transfer
            .channel
            .as_mut()
            .ok_or(SessionError::UnexpectedMessage)?;
        if !channel.io_admitted() || channel.peer_identity() != &binding.device {
            return Err(SessionError::UnexpectedMessage);
        }
        let store = self.sync_store(&binding.device);
        let final_chunk = file.final_chunk.ok_or(SessionError::UnexpectedMessage)?;
        match transfer.phase {
            FilePhase::Manifest => {
                let conversation = super::conversation::dm_conversation_id(
                    &self.identity.public(),
                    &binding.device,
                );
                if transfer.sync.step(channel, &store, conversation).await? {
                    transfer.sync = super::session::SyncProgress::default();
                    transfer.phase = if transfer.legacy {
                        FilePhase::Chunks
                    } else {
                        FilePhase::ManifestCustody
                    };
                }
            }
            FilePhase::ManifestCustody => {
                let conversation = super::conversation::dm_conversation_id(
                    &self.identity.public(),
                    &binding.device,
                );
                if !transfer.legacy {
                    match super::session::request_durable_have(
                        channel,
                        conversation,
                        binding.event_id,
                    )
                    .await
                    {
                        Ok(true) if self.file_destination_current(file, destination) => {
                            let mut delivery =
                                self.delivery.lock().expect("delivery lock not poisoned");
                            if delivery.completed_work(file.id).is_some() {
                                delivery
                                    .retire_destination(file.id, binding.event_id)
                                    .map_err(|error| {
                                        SessionError::Serialization(error.to_string())
                                    })?;
                            }
                        }
                        Ok(_) => {}
                        Err(_) => {
                            transfer.channel = None;
                            transfer.legacy = true;
                            transfer.sync = super::session::SyncProgress::default();
                            transfer.phase = FilePhase::Dial;
                            return Ok(false);
                        }
                    }
                }
                transfer.phase = if transfer.legacy {
                    FilePhase::Chunks
                } else {
                    FilePhase::Consent
                };
            }
            FilePhase::Consent => {
                match self
                    .query_file_consent(
                        channel,
                        &binding.device,
                        file.id,
                        file.file_conversation,
                        binding.event_id,
                    )
                    .await
                {
                    Ok(status) => {
                        if status == crate::eventlog::sync::FileConsentStatus::Accepted {
                            transfer.phase = FilePhase::InitialCustody;
                        } else {
                            transfer.channel = None;
                            transfer.phase = FilePhase::ConsentWait;
                        }
                    }
                    Err(_) => {
                        // Pre-consent clients do not understand the appended query.
                        // Their old automatic transfer path remains available.
                        transfer.channel = None;
                        transfer.legacy = true;
                        transfer.sync = super::session::SyncProgress::default();
                        transfer.phase = FilePhase::Dial;
                    }
                }
            }
            FilePhase::InitialCustody => {
                if super::session::request_durable_have(
                    channel,
                    file.file_conversation,
                    final_chunk,
                )
                .await?
                {
                    return Ok(self.file_destination_current(file, destination));
                }
                transfer.phase = FilePhase::Chunks;
            }
            FilePhase::Chunks => {
                let complete = transfer
                    .sync
                    .step_burst(
                        channel,
                        &store,
                        file.file_conversation,
                        super::session::FILE_FOLLOWUP_BURST,
                    )
                    .await?;
                if let Some(confirmed) = transfer.sync.remote_have_count() {
                    let confirmed = confirmed.min(file.chunk_count as usize) as u32;
                    let previous = self
                        .outgoing_file_acks
                        .lock()
                        .expect("outgoing file progress lock not poisoned")
                        .insert((file.id, binding.event_id), confirmed);
                    if previous != Some(confirmed) {
                        self.note_file_change();
                    }
                }
                if complete {
                    transfer.phase = FilePhase::FinalCustody;
                }
            }
            FilePhase::FinalCustody => {
                if super::session::request_durable_have(
                    channel,
                    file.file_conversation,
                    final_chunk,
                )
                .await?
                {
                    return Ok(self.file_destination_current(file, destination));
                }
                return Err(SessionError::NoProgress);
            }
            FilePhase::Dial
            | FilePhase::ConsentWait
            | FilePhase::RelayManifestLegacy
            | FilePhase::RelayManifestFederated
            | FilePhase::RelayChunksLegacy
            | FilePhase::RelayChunksFederated
            | FilePhase::RelayCustody
            | FilePhase::RelayHeld => unreachable!(),
        }
        Ok(false)
    }

    async fn advance_relay_file_transfer(
        &self,
        transfer: &mut FileTransfer,
    ) -> Result<bool, SessionError> {
        use crate::postoffice::opaque::RelayMailbox;
        let file = &transfer.file;
        let destination = &transfer.destination;
        let binding = &destination.binding;
        let (_, relay) = transfer
            .route
            .as_ref()
            .ok_or(SessionError::UnexpectedMessage)?;
        let current = || {
            self.file_destination_current(file, destination)
                && self.relay_send_allowed(&relay.public)
                && self.outgoing_file_consent_status(file.id, binding.event_id)
                    == Some(crate::eventlog::sync::FileConsentStatus::Accepted)
                && self
                    .roster
                    .lock()
                    .expect("roster lock not poisoned")
                    .get(&relay.public.user_id())
                    .is_some_and(|peer| {
                        peer.post_office
                            && peer.public == relay.public
                            && peer.account_id == relay.account_id
                    })
        };
        let channel = transfer
            .channel
            .as_mut()
            .ok_or(SessionError::UnexpectedMessage)?;
        if !current() || !channel.io_admitted() || channel.peer_identity() != &relay.public {
            return Err(SessionError::UnexpectedMessage);
        }
        if matches!(transfer.phase, FilePhase::RelayHeld) {
            return Ok(false);
        }
        if matches!(transfer.phase, FilePhase::RelayCustody) {
            let (mailbox, final_chunk) = transfer
                .relay_custody
                .ok_or(SessionError::UnexpectedMessage)?;
            if !super::session::request_durable_have(channel, mailbox, final_chunk).await?
                || !current()
            {
                return Err(SessionError::NoProgress);
            }
            // Exact custody consumed the terminal reply. Reuse this clean
            // sync channel without claiming recipient completion.
            if let Some(channel) = transfer.channel.take() {
                self.recycle_sync_channel(channel);
            }
            transfer.phase = FilePhase::RelayHeld;
            return Ok(false);
        }
        let federated = matches!(
            transfer.phase,
            FilePhase::RelayManifestFederated | FilePhase::RelayChunksFederated
        );
        if federated && !self.bridge_directory_allowed() {
            return Err(SessionError::UnexpectedMessage);
        }
        let chunks = matches!(
            transfer.phase,
            FilePhase::RelayChunksLegacy | FilePhase::RelayChunksFederated
        );
        if transfer.opaque.is_none() {
            let mailbox = if federated {
                RelayMailbox::federated(&self.identity, &binding.device)
            } else {
                RelayMailbox::new(&self.identity, &binding.device, &relay.public)
            }
            .map_err(|error| SessionError::Serialization(error.to_string()))?;
            let conversation = if chunks {
                file.file_conversation
            } else {
                super::conversation::dm_conversation_id(&self.identity.public(), &binding.device)
            };
            transfer.opaque = Some(super::opaque_relay::OpaqueMailboxProgress::new(
                &relay.public,
                &binding.device,
                Some(conversation),
                mailbox,
            ));
        }
        let opaque = transfer.opaque.as_mut().unwrap();
        let complete = self
            .advance_opaque_mailbox_if(channel, opaque, &|| {
                current() && (!federated || self.bridge_directory_allowed())
            })
            .await?;
        if !current() || (federated && !self.bridge_directory_allowed()) {
            return Err(SessionError::UnexpectedMessage);
        }
        if complete {
            if chunks {
                let final_chunk = file.final_chunk.ok_or(SessionError::UnexpectedMessage)?;
                let outer = opaque
                    .uploaded()
                    .iter()
                    .find(|(inner, _)| *inner == final_chunk)
                    .map(|(_, outer)| *outer)
                    .ok_or(SessionError::NoProgress)?;
                let mailbox = if federated {
                    RelayMailbox::federated(&self.identity, &binding.device)
                } else {
                    RelayMailbox::new(&self.identity, &binding.device, &relay.public)
                }
                .map_err(|error| SessionError::Serialization(error.to_string()))?;
                transfer.relay_custody = Some((mailbox.id(), outer));
            }
            transfer.opaque = None;
            transfer.phase = match transfer.phase {
                FilePhase::RelayManifestLegacy if self.bridge_directory_allowed() => {
                    FilePhase::RelayManifestFederated
                }
                FilePhase::RelayManifestLegacy | FilePhase::RelayManifestFederated => {
                    FilePhase::RelayChunksLegacy
                }
                FilePhase::RelayChunksLegacy if self.bridge_directory_allowed() => {
                    FilePhase::RelayChunksFederated
                }
                FilePhase::RelayChunksLegacy | FilePhase::RelayChunksFederated => {
                    FilePhase::RelayCustody
                }
                _ => return Err(SessionError::UnexpectedMessage),
            };
        }
        Ok(false)
    }

    async fn retry_file_destination_inner(
        &self,
        file: &super::delivery_store::FileCard,
        destination: &super::delivery_store::FileDestination,
        direct: bool,
    ) -> bool {
        let binding = &destination.binding;
        let current = || self.file_destination_current(file, destination);
        if !current() {
            return false;
        }
        if self.recipient_completed_file(file, destination) {
            return true;
        }
        let manifest_conv =
            super::conversation::dm_conversation_id(&self.identity.public(), &binding.device);
        let mut candidates = Vec::new();
        if direct && let Some(peer) = self.routing_peer(&binding.device.user_id()) {
            candidates.push((false, peer));
        }
        let consented = self.outgoing_file_consent_status(file.id, binding.event_id)
            == Some(crate::eventlog::sync::FileConsentStatus::Accepted);
        if consented {
            let roster = self.roster.lock().expect("roster lock not poisoned");
            candidates.extend(
                self.post_office_candidates_for(&roster, &binding.device)
                    .into_iter()
                    .map(|peer| (true, peer)),
            );
        }
        for (relay, peer) in candidates {
            if (relay && !self.relay_send_allowed(&peer.public))
                || (!relay && (peer.public != binding.device || peer.account_id != binding.account))
            {
                continue;
            }
            #[cfg(test)]
            let phase = std::sync::atomic::AtomicU8::new(0);
            let outcome = tokio::time::timeout(FILE_EXCHANGE_TIMEOUT, async {
                let Ok(mut channel) = self.privacy_dial(peer.addr, &peer.public).await else {
                    return false;
                };
                #[cfg(test)]
                phase.store(1, std::sync::atomic::Ordering::Relaxed);
                if !current()
                    || (relay && !self.relay_send_allowed(&peer.public))
                    || channel
                        .peer_announcement()
                        .is_some_and(|proof| proof.account_id() != peer.account_id)
                {
                    return false;
                }
                if relay {
                    if self
                        .opaque_relay_round(&mut channel, &binding.device, Some(manifest_conv))
                        .await
                        .is_err()
                    {
                        return false;
                    }
                    #[cfg(test)]
                    phase.store(6, std::sync::atomic::Ordering::Relaxed);
                    let Ok(uploaded) = self
                        .opaque_relay_round(
                            &mut channel,
                            &binding.device,
                            Some(file.file_conversation),
                        )
                        .await
                    else {
                        return false;
                    };
                    let Some(final_chunk) = file.final_chunk else {
                        return false;
                    };
                    let Some((_, outer_id)) = uploaded
                        .iter()
                        .find(|(inner_id, _)| *inner_id == final_chunk)
                    else {
                        return false;
                    };
                    let Ok(mailbox) = crate::postoffice::opaque::RelayMailbox::new(
                        &self.identity,
                        &binding.device,
                        &peer.public,
                    ) else {
                        return false;
                    };
                    #[cfg(test)]
                    phase.store(7, std::sync::atomic::Ordering::Relaxed);
                    return super::session::request_durable_have(
                        &mut channel,
                        mailbox.id(),
                        *outer_id,
                    )
                    .await
                    .unwrap_or(false)
                        && current();
                }
                let store = self.sync_store(channel.peer_identity());
                // Publish the manifest first so private recipients/relays can
                // durably reconstruct chunk scope before any chunk disclosure.
                if super::session::request_round(&mut channel, &store, manifest_conv)
                    .await
                    .is_err()
                {
                    return false;
                }
                if !current() || (relay && !self.relay_send_allowed(&peer.public)) {
                    return false;
                }
                if !relay {
                    match self
                        .query_file_consent(
                            &mut channel,
                            &peer.public,
                            file.id,
                            file.file_conversation,
                            binding.event_id,
                        )
                        .await
                    {
                        Ok(status) => {
                            if status != crate::eventlog::sync::FileConsentStatus::Accepted {
                                return false;
                            }
                        }
                        Err(_) => {
                            // Reconnect before speaking the original chunk protocol.
                            let Ok(reconnected) = self.privacy_dial(peer.addr, &peer.public).await
                            else {
                                return false;
                            };
                            channel = reconnected;
                            if super::session::request_round(&mut channel, &store, manifest_conv)
                                .await
                                .is_err()
                            {
                                return false;
                            }
                        }
                    }
                }
                let Some(final_chunk) = file.final_chunk else {
                    return false;
                };
                // A target may have verified/saved the file and reclaimed chunks
                // while its early card receipt was already in flight.
                if !relay {
                    #[cfg(test)]
                    phase.store(2, std::sync::atomic::Ordering::Relaxed);
                    match super::session::request_durable_have(
                        &mut channel,
                        file.file_conversation,
                        final_chunk,
                    )
                    .await
                    {
                        Ok(true) => return current(),
                        Ok(false) => {}
                        Err(_) => {
                            #[cfg(test)]
                            phase.store(3, std::sync::atomic::Ordering::Relaxed);
                            // Legacy endpoints may close on the optional probe.
                            // Reconnect and preserve manifest-first ordinary transfer.
                            let Ok(reconnected) = self.privacy_dial(peer.addr, &peer.public).await
                            else {
                                return false;
                            };
                            channel = reconnected;
                            if !current()
                                || channel
                                    .peer_announcement()
                                    .is_some_and(|proof| proof.account_id() != peer.account_id)
                            {
                                return false;
                            }
                            if super::session::request_round(&mut channel, &store, manifest_conv)
                                .await
                                .is_err()
                            {
                                return false;
                            }
                        }
                    }
                }
                #[cfg(test)]
                phase.store(4, std::sync::atomic::Ordering::Relaxed);
                if super::session::request_file_round(&mut channel, &store, file.file_conversation)
                    .await
                    .is_err()
                {
                    return false;
                }
                #[cfg(test)]
                phase.store(5, std::sync::atomic::Ordering::Relaxed);
                let held = super::session::request_durable_have(
                    &mut channel,
                    file.file_conversation,
                    final_chunk,
                )
                .await
                .unwrap_or(false);
                held && current() && (!relay || self.relay_send_allowed(&peer.public))
            })
            .await;
            #[cfg(test)]
            if !matches!(outcome, Ok(true)) {
                let stage = [
                    "dial",
                    "manifest",
                    "initial-custody",
                    "legacy-reconnect",
                    "chunks",
                    "final-custody",
                    "relay-chunks",
                    "relay-custody",
                ][phase.load(std::sync::atomic::Ordering::Relaxed) as usize];
                eprintln!(
                    "file-attempt failure stage={stage} timeout={} relay={relay}",
                    outcome.is_err()
                );
            }
            let held = outcome.unwrap_or(false);
            if held {
                return !relay;
            }
        }
        false
    }

    pub(in crate::node) async fn flush_file_delivery(&self, id: EventId) {
        let file = {
            let mut store = self.delivery.lock().expect("delivery lock not poisoned");
            if self.recover_delivery(&mut store).is_err() {
                return;
            }
            store.file_card(id).cloned()
        };
        let Some(file) = file else {
            return;
        };
        for destination in &file.destinations {
            if destination.active
                && self.retry_file_destination(&file, destination).await
                && self
                    .delivery
                    .lock()
                    .expect("delivery lock not poisoned")
                    .retire_file_destination(file.id, destination.binding.event_id)
                    .is_ok()
            {
                self.outgoing_file_acks
                    .lock()
                    .expect("outgoing file progress lock not poisoned")
                    .remove(&(file.id, destination.binding.event_id));
            }
        }
    }
}

#[cfg(test)]
mod diagnostic_tests {
    #[tokio::test]
    async fn bounded_relay_batches_eventually_visit_a_healthy_tail_after_four_failures() {
        use crate::discovery::Announce;
        use crate::identity::{account::Account, device::DeviceIdentity};
        let dir = tempfile::tempdir().unwrap();
        let peer = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "Peer",
            9,
        );
        let (node, _, _) = super::super::test_support::node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &peer,
        );
        for _ in 0..10 {
            let relay = Announce::new_post_office(&DeviceIdentity::generate(), "Relay", 9);
            node.roster.lock().unwrap().update(
                &relay,
                "127.0.0.1".parse().unwrap(),
                &node.user_id(),
            );
        }
        let candidates = node.post_office_candidates(&node.roster.lock().unwrap());
        assert_eq!(candidates.len(), 10);
        let tail = candidates.last().unwrap().public.clone();
        let first = node.relay_candidate_batch("pull:peer", candidates.clone());
        assert_eq!(first.len(), 4);
        assert!(!first.iter().any(|peer| peer.public == tail));
        let second = node.relay_candidate_batch("pull:peer", candidates.clone());
        assert_eq!(second.len(), 4);
        let third = node.relay_candidate_batch("pull:peer", candidates);
        assert!(
            third.iter().any(|peer| peer.public == tail),
            "failed head batches must not hide later healthy relays"
        );
        // Other peers and operation lanes start independently.
        assert_eq!(
            node.relay_candidate_batch("upload:peer", first.clone())[0].public,
            first[0].public
        );
    }

    #[tokio::test]
    async fn file_exchange_accepts_a_valid_route_slower_than_four_hundred_milliseconds() {
        use super::super::test_support::node_with_files;
        use crate::discovery::Announce;
        use crate::identity::{account::Account, device::DeviceIdentity};
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let aa = Account::generate();
        let bob_account = Account::generate();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
        let bp = Announce::new_with_account(
            &bob,
            &bob_account,
            "Bob",
            listener.local_addr().unwrap().port(),
        );
        let (a, _, _) = node_with_files(source.path(), alice, aa, &bp);
        let (b, _, _) = node_with_files(target.path(), bob, bob_account, &ap);
        b.set_attachment_global_auto(true).unwrap();
        let path = source.path().join("routed.bin");
        std::fs::write(&path, b"valid slow route").unwrap();
        let (id, file) = a
            .enqueue_file_to_account(&b.account_id(), &path, crate::file::FileKind::File)
            .await
            .unwrap();
        let server_node = b.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            if let Ok(channel) = server_node.privacy_accept(stream).await {
                server_node.serve_connection(channel).await;
            }
        });
        let card = a.delivery.lock().unwrap().file_card(id).unwrap().clone();
        let destination = card.destinations[0].clone();
        let mut scheduler = super::FileDeliveryScheduler::default();
        scheduler.admit(&a, vec![(card, destination)], None);
        let mut diagnostics = super::DeliveryDiagnosticThrottle::default();
        let complete = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                scheduler.advance(&a);
                if let Some(result) = scheduler.workers.join_next().await {
                    scheduler.finish(&a, result, &mut diagnostics);
                }
                if a.outgoing_file_progress(id)
                    .is_some_and(|progress| progress.delivered)
                {
                    break;
                }
                if scheduler.workers.is_empty() && scheduler.transfers.is_empty() {
                    tokio::task::yield_now().await;
                }
            }
        })
        .await;
        server.abort();
        let _ = server.await;
        assert!(
            complete.is_ok(),
            "scheduler fairness must not reject a healthy 600ms handshake"
        );
        assert_eq!(b.read_file(file).unwrap(), b"valid slow route");
    }

    #[tokio::test]
    async fn pending_consent_releases_file_scheduler_capacity_without_self_waking() {
        use super::super::test_support::node_with_files;
        use crate::discovery::Announce;
        use crate::identity::{account::Account, device::DeviceIdentity};
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let aa = Account::generate();
        let bob_account = Account::generate();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
        let bp = Announce::new_with_account(
            &bob,
            &bob_account,
            "Bob",
            listener.local_addr().unwrap().port(),
        );
        let (a, _, _) = node_with_files(source.path(), alice, aa, &bp);
        let (b, _, _) = node_with_files(target.path(), bob, bob_account, &ap);
        let path = source.path().join("waiting.bin");
        std::fs::write(&path, b"offer awaiting approval").unwrap();
        let (id, _) = a
            .enqueue_file_to_account(&b.account_id(), &path, crate::file::FileKind::File)
            .await
            .unwrap();
        let server = tokio::spawn(b.run_accept_loop(listener));
        let card = a.delivery.lock().unwrap().file_card(id).unwrap().clone();
        let destination = card.destinations[0].clone();
        let mut scheduler = super::FileDeliveryScheduler::default();
        scheduler.admit(&a, vec![(card, destination)], None);
        let mut diagnostics = super::DeliveryDiagnosticThrottle::default();
        for _ in 0..12 {
            scheduler.advance(&a);
            if let Some(result) = scheduler.workers.join_next().await {
                scheduler.finish(&a, result, &mut diagnostics);
            }
            if scheduler.transfers.is_empty() {
                break;
            }
        }
        server.abort();
        let _ = server.await;
        assert!(
            scheduler.transfers.is_empty(),
            "pending consent must release sockets and scheduling slots"
        );
        assert_eq!(
            scheduler.relay_cooldowns.len(),
            1,
            "waiting offers need a bounded retry deadline"
        );
    }

    #[test]
    fn delivery_local_failure_diagnostic_throttle_has_fixed_non_sliding_deadline() {
        use std::time::{Duration, Instant};
        let mut throttle = super::DeliveryDiagnosticThrottle::default();
        let start = Instant::now();
        assert!(throttle.admit(start));
        for millis in [0, 500, 1_000, 29_999] {
            assert!(!throttle.admit(start + Duration::from_millis(millis)));
        }
        assert!(throttle.admit(start + Duration::from_secs(30)));
        assert!(!throttle.admit(start + Duration::from_secs(30)));
        assert!(throttle.admit(start + Duration::from_secs(60)));
    }
    #[tokio::test]
    async fn revoked_verification_snapshot_cannot_start_proactive_delivery_pull() {
        use crate::discovery::Announce;
        use crate::identity::{account::Account, device::DeviceIdentity};
        use std::time::Duration;
        use tokio::net::TcpListener;
        let dir = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(
            &peer,
            &Account::generate(),
            "Peer",
            listener.local_addr().unwrap().port(),
        );
        let (sender, _, _) = super::super::test_support::node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        sender.replace_verified_dm_pull_peers(vec![proof.user_id.clone()]);
        // A worker can hold an owned peer snapshot while the host revokes manual
        // verification before the scheduled task first runs.
        let pending = sender.reconcile_delivery_peer(&proof);
        sender.replace_verified_dm_pull_peers(Vec::new());
        let (finished, incoming) = tokio::join!(
            tokio::time::timeout(Duration::from_secs(1), pending),
            tokio::time::timeout(Duration::from_secs(1), listener.accept()),
        );
        assert!(
            incoming.is_err(),
            "revoked verification must prevent a new proactive recovery connection"
        );
        assert!(finished.is_ok());
    }
}
