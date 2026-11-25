//! Atomic DM acceptance and control receipt dispatch. Network hooks take owned
//! immutable snapshots; no delivery or ratchet guard crosses a transport await.
use super::delivery_store::{DeliveryDestination, ReceiptDelivery};
use super::delivery_store::{DeliveryStore, DeliveryTransaction, OutgoingDelivery};
#[cfg(test)]
use super::sentlog::SentEntry;
use super::session::SessionError;
use super::*;
use crate::eventlog::{ConversationId, Event, EventId, LogError};
use crate::identity::device::PublicIdentity;
use std::sync::Arc;
use std::sync::atomic::Ordering;

// Owned by the delivery loop: cancellation drops every retained channel. Keep
// eight scheduling slots, at most six retained sockets (one outbound permit
// reserved for controls), and discard idle or unauthorized state.
#[cfg(test)]
thread_local! {
    static FILE_RETRY_ROUTE_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

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
const FILE_RELAY_RETRY: std::time::Duration = std::time::Duration::from_secs(300);

// Whole authenticated control exchanges need several routed round trips. Keep
// them off the serial file scheduler, with independent capacity for each lane.
// Successful absence of new work, including already-confirmed relay custody,
// needs sparse repair rather than another short progress/retry turn.
pub(super) const QUIET_REPAIR_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(5 * 60);

const CONTROL_EXCHANGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeliveryAttemptContext {
    ImmediateFlush,
    RetainedRecovery,
    ReceiptRecovery,
}

/// Transport completion is separate from accepted data and from retryable failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::node) enum Reconciliation {
    Advanced,
    Quiet,
    /// A clean bounded exchange has retained work; no storage/delivery claim.
    More,
    /// Source policy/capacity blocks progress; retain pending state for sparse repair.
    Limited,
    Deferred,
}
impl Reconciliation {
    fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Advanced, _) | (_, Self::Advanced) => Self::Advanced,
            (Self::More, _) | (_, Self::More) => Self::More,
            (Self::Deferred, _) | (_, Self::Deferred) => Self::Deferred,
            (Self::Limited, _) | (_, Self::Limited) => Self::Limited,
            _ => Self::Quiet,
        }
    }
    fn schedule<K: Eq + std::hash::Hash + Clone, R: Eq + Clone>(
        self,
        retries: &mut super::delivery_retry::RetrySchedule<K, R>,
        key: K,
        routes: R,
        now: std::time::Instant,
    ) {
        match self {
            Self::More => retries.settled(key, routes, std::time::Duration::from_millis(500), now),
            Self::Limited => retries.settled(key, routes, QUIET_REPAIR_INTERVAL, now),
            Self::Quiet => retries.settled(key, routes, QUIET_REPAIR_INTERVAL, now),
            other => retries.finished(key, routes, other == Self::Advanced, now),
        }
    }
    fn receipt_result(custody: bool, failed: bool, continuation: TransferContinuation) -> Self {
        if failed {
            Self::Deferred
        } else if custody {
            Self::Advanced
        } else {
            match continuation {
                TransferContinuation::None => Self::Deferred,
                TransferContinuation::More => Self::More,
                TransferContinuation::Limited => Self::Limited,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransferOutcome {
    Held,
    More,
    Limited,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransferContinuation {
    None,
    More,
    Limited,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum ControlWork {
    Destination(EventId),
    Receipt(EventId),
    Pull(String),
}

// Scheduling inputs only. Actual dials keep the current preferred address and
// authenticate the exact physical endpoint through the existing transport owner.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct RelayRouteInput {
    ed25519: [u8; 32],
    x25519: [u8; 32],
    account: Option<String>,
    post_office: bool,
    endpoints: crate::discovery::roster::SchedulingEndpoints,
}
impl RelayRouteInput {
    fn observe(peer: &crate::discovery::roster::PeerRecord) -> Self {
        Self {
            ed25519: peer.public.ed25519_pub,
            x25519: peer.public.x25519_pub,
            account: peer.account_id.clone(),
            post_office: peer.post_office,
            endpoints: peer.scheduling_endpoints(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ControlRouteInput {
    endpoints: std::collections::BTreeSet<(String, std::net::SocketAddr)>,
    relays: std::collections::BTreeSet<RelayRouteInput>,
    publicly_observed: bool,
    policy_generation: u64,
    demand_generation: u64,
}

struct ControlRoutes {
    peer: PublicIdentity,
    relays: std::collections::BTreeSet<RelayRouteInput>,
    endpoints: std::collections::BTreeSet<(String, std::net::SocketAddr)>,
    publicly_observed: bool,
    policy_generation: u64,
    demand_generation: u64,
}

/// Task IDs distinguish an aborted route owner from its same-key replacement.
#[derive(Default)]
struct ControlTaskOwners {
    current: std::collections::HashMap<ControlWork, tokio::task::AbortHandle>,
    owners: std::collections::HashMap<tokio::task::Id, ControlWork>,
}
impl ControlTaskOwners {
    fn insert(&mut self, key: ControlWork, task: tokio::task::AbortHandle) {
        self.owners.insert(task.id(), key.clone());
        self.current.insert(key, task);
    }
    fn finished(&mut self, task: tokio::task::Id) -> Option<ControlWork> {
        let key = self.owners.remove(&task)?;
        if self
            .current
            .get(&key)
            .is_some_and(|current| current.id() == task)
        {
            self.current.remove(&key);
            Some(key)
        } else {
            None
        }
    }
    fn cancel(&mut self, key: &ControlWork) {
        if let Some(task) = self.current.remove(key) {
            task.abort();
        }
    }
    fn clear(&mut self) {
        self.current.clear();
        self.owners.clear();
    }
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
    fn retry_routes(&self) -> ControlRouteInput {
        ControlRouteInput {
            endpoints: self.endpoints.clone(),
            relays: self.relays.clone(),
            publicly_observed: self.publicly_observed,
            policy_generation: self.policy_generation,
            demand_generation: self.demand_generation,
        }
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
    sticker_proof_attempted: bool,
    identity_offered: bool,
    last_used: std::time::Instant,
    candidate_cursor: usize,
    candidate_count: usize,
    route: Option<(bool, crate::discovery::roster::PeerRecord)>,
    opaque: Option<super::opaque_relay::OpaqueMailboxProgress>,
    relay_custody: Option<(ConversationId, EventId)>,
    held_observation: Option<RelayHeldObservation>,
    recipient_observation: Option<HeldPeer>,
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
            sticker_proof_attempted: false,
            identity_offered: false,
            last_used: std::time::Instant::now(),
            candidate_cursor: 0,
            candidate_count: 0,
            route: None,
            opaque: None,
            relay_custody: None,
            held_observation: None,
            recipient_observation: None,
        }
    }
}

type FileKey = (EventId, EventId);
type FileStepResult = (
    FileKey,
    std::collections::HashMap<FileKey, FileTransfer>,
    FileStepCompletion,
);
#[derive(Clone, Copy, Debug)]
enum FileStepOutcome {
    Clean(bool),
    Failed,
}

impl FileStepOutcome {
    fn complete(&self) -> bool {
        matches!(self, Self::Clean(true))
    }
}

#[derive(Debug)]
struct FileStepCompletion {
    token: u64,
    routes: FileRoutes,
    outcome: FileStepOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileRoutes {
    relays: std::collections::BTreeSet<RelayRouteInput>,
    policy_generation: u64,
    endpoints: std::collections::BTreeSet<(bool, String, Option<String>, std::net::SocketAddr)>,
    current: bool,
    custody: bool,
}

// Only cursor/scalar state survives a failed socket. Fresh journal admission
// supplies the FileCard (including fanout), destination and authorization.
struct FileContinuation {
    routes: FileRoutes,
    recipient_observation: Option<HeldPeer>,
    candidate_cursor: usize,
    legacy: bool,
    sticker_proof_attempted: bool,
    identity_offered: bool,
}

type FileWork = (
    super::delivery_store::FileCard,
    super::delivery_store::FileDestination,
);

/// The worker owns file scheduling state across wakeups. A scan takes only a
/// cursor and a capacity snapshot, so no socket or mutable cache crosses the
/// blocking journal read.
#[derive(Clone)]
struct HeldPeer {
    public: crate::identity::device::PublicIdentity,
    account: Option<String>,
    observation: Option<super::participant_runtime::FilePullPeer>,
}
struct RelayHeldObservation {
    peers: [HeldPeer; 2],
    policy_generation: u64,
}
impl RelayHeldObservation {
    fn capture(
        node: &Node,
        destination: &super::delivery_store::FileDestination,
        relay: &crate::discovery::roster::PeerRecord,
        now: std::time::Instant,
    ) -> Self {
        let mut value = Self {
            peers: [
                HeldPeer {
                    public: destination.binding.device.clone(),
                    account: destination.binding.account.clone(),
                    observation: None,
                },
                HeldPeer {
                    public: relay.public.clone(),
                    account: relay.account_id.clone(),
                    observation: None,
                },
            ],
            policy_generation: node.privacy.generation.load(Ordering::Acquire),
        };
        let roster = node.roster.lock().expect("roster lock not poisoned");
        value.refresh(&roster, now);
        value
    }
    // Roster only: callers must release this guard before policy or storage work.
    fn refresh(
        &mut self,
        roster: &crate::discovery::roster::Roster,
        now: std::time::Instant,
    ) -> bool {
        let mut changed = false;
        for peer in &mut self.peers {
            let next = roster
                .get(&peer.public.user_id())
                .filter(|record| record.public == peer.public && record.account_id == peer.account)
                .map(|record| super::participant_runtime::FilePullPeer::observe(record, now));
            changed |= match (&peer.observation, &next) {
                (Some(old), Some(next)) => old.changes_work(next, now),
                (None, None) => false,
                _ => true,
            };
            peer.observation = next;
        }
        changed
    }
}
enum FileHoldKind {
    ConsentWait,
    RelayHeld(Box<RelayHeldObservation>),
}
struct FileHold {
    key: FileKey,
    until: std::time::Instant,
    kind: FileHoldKind,
}

#[derive(Default)]
struct FileDeliveryScheduler {
    pending_cursor: Option<FileKey>,
    active_cursor: Option<FileKey>,
    transfers: std::collections::HashMap<FileKey, FileTransfer>,
    workers: tokio::task::JoinSet<FileStepResult>,
    running_sockets: std::collections::HashSet<FileKey>,
    retries: super::delivery_retry::RetrySchedule<FileKey, FileRoutes>,
    parked: std::collections::HashMap<FileKey, FileContinuation>,
    // Bounded scheduling hints, not custody/delivery truth. Eviction merely
    // permits an earlier retry after other destinations have had their turn.
    relay_cooldowns: std::collections::VecDeque<FileHold>,
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
        self.scan_at(node, std::time::Instant::now())
    }

    fn refresh_held(&mut self, node: &Node, now: std::time::Instant) -> bool {
        let before = self.relay_cooldowns.len();
        let roster = node.roster.lock().expect("roster lock not poisoned");
        self.relay_cooldowns
            .retain_mut(|hold| match &mut hold.kind {
                FileHoldKind::ConsentWait => true,
                FileHoldKind::RelayHeld(observation) => !observation.refresh(&roster, now),
            });
        self.relay_cooldowns.len() < before
    }

    fn scan_at(&mut self, node: &Node, now: std::time::Instant) -> FileScan {
        node.prune_file_transfers(&mut self.transfers);
        let generation = node.privacy.generation.load(Ordering::Acquire);
        self.relay_cooldowns.retain(|hold| {
            let key = hold.key;
            hold.until > now
                && match &hold.kind {
                    FileHoldKind::RelayHeld(observation) => {
                        observation.policy_generation == generation
                    }
                    FileHoldKind::ConsentWait => true,
                }
                && (!matches!(hold.kind, FileHoldKind::ConsentWait)
                    || match node.outgoing_file_consent_status(key.0, key.1) {
                        Ok(status) => {
                            status != Some(crate::eventlog::sync::FileConsentStatus::Accepted)
                        }
                        Err(error) => {
                            log::warn!("file consent retry observation failed: {error}");
                            true
                        }
                    })
        });
        node.prune_outgoing_file_acks();
        FileScan {
            cursor: self.pending_cursor,
            slots: MAX_FILE_TRANSFERS - self.transfers.len() - self.workers.len(),
        }
    }

    fn admit(&mut self, node: &Node, files: Vec<FileWork>, cursor: Option<FileKey>) {
        self.admit_at(node, files, cursor, std::time::Instant::now());
    }

    fn admit_at(
        &mut self,
        node: &Node,
        files: Vec<FileWork>,
        cursor: Option<FileKey>,
        now: std::time::Instant,
    ) {
        self.pending_cursor = cursor;
        node.prune_file_transfers(&mut self.transfers);
        for (file, destination) in files {
            let key = (file.id, destination.binding.event_id);
            // Scan owns expiry and consent wakeups. A retained hold cannot
            // admit work, so avoid preparing routes and reading consent for it.
            if self.relay_cooldowns.iter().any(|hold| hold.key == key) {
                continue;
            }
            let routes = node.file_retry_routes(&file, &destination);
            if !routes.current {
                self.parked.remove(&key);
                // The in-flight worker still owns its matching completion and
                // socket marker even after authority is revoked. Forget only
                // inactive hints; otherwise completion cannot release its owner.
                if !self.running_sockets.contains(&key) {
                    self.retries.forget(&key);
                }
                continue;
            }
            if self.transfers.len() + self.workers.len() < MAX_FILE_TRANSFERS
                && self.retries.ready(&key, &routes, now)
                && node.file_destination_current(&file, &destination)
                && !self.transfers.contains_key(&key)
                && !self.running_sockets.contains(&key)
            {
                let mut transfer = FileTransfer::new(file, destination);
                if let Some(continuation) = self
                    .parked
                    .remove(&key)
                    .filter(|previous| previous.routes == routes)
                {
                    transfer.recipient_observation = continuation.recipient_observation;
                    transfer.candidate_cursor = continuation.candidate_cursor;
                    transfer.legacy = continuation.legacy;
                    transfer.sticker_proof_attempted = continuation.sticker_proof_attempted;
                    transfer.identity_offered = continuation.identity_offered;
                }
                self.transfers.insert(key, transfer);
            }
        }
    }

    fn advance(&mut self, node: &std::sync::Arc<Node>) {
        self.advance_at(node, std::time::Instant::now());
    }

    fn advance_at(&mut self, node: &std::sync::Arc<Node>, now: std::time::Instant) {
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
            self.active_cursor = Some(key);
            let Some(work) = node.runtime_work.admit() else {
                return;
            };
            let routes = node.file_retry_routes(&transfer.file, &transfer.destination);
            let Some((token, evicted)) = self.retries.reserve(key, routes.clone(), now) else {
                // Cooled/capacity-deferred keys retain durable intent but own no
                // transfer or socket slot. The scan cursor already passed them.
                self.transfers.remove(&key);
                continue;
            };
            if let Some(evicted) = evicted {
                self.parked.remove(&evicted);
            }
            if dial {
                sockets += 1;
            }
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
            self.workers.spawn(work.track(async move {
                let mut transfers = std::collections::HashMap::from([(key, transfer)]);
                let outcome = node
                    .retry_file_step_outcome(&file, &destination, &mut transfers)
                    .await;
                (
                    key,
                    transfers,
                    FileStepCompletion {
                        token,
                        routes,
                        outcome,
                    },
                )
            }));
        }
    }

    fn finish(
        &mut self,
        node: &Node,
        result: Result<FileStepResult, tokio::task::JoinError>,
        diagnostic: &mut DeliveryDiagnosticThrottle,
    ) {
        self.finish_at(node, result, diagnostic, std::time::Instant::now());
    }

    fn finish_at(
        &mut self,
        node: &Node,
        result: Result<FileStepResult, tokio::task::JoinError>,
        diagnostic: &mut DeliveryDiagnosticThrottle,
        now: std::time::Instant,
    ) {
        let Ok((key, mut transfers, completion)) = result else {
            // Never retain reservations after a panicked child; drop cancels the
            // other workers and the owning accept loop retires on this failure.
            self.running_sockets.clear();
            self.workers.abort_all();
            return;
        };
        if !self.retries.owns(&key, completion.token) {
            return;
        }
        self.running_sockets.remove(&key);
        let mut failed = matches!(completion.outcome, FileStepOutcome::Failed);
        if completion.outcome.complete() {
            if node
                .delivery
                .lock()
                .expect("delivery lock not poisoned")
                .retire_file_destination(key.0, key.1)
                .is_err()
            {
                diagnostic.warn_if_due();
                failed = true;
            } else {
                node.delivery_notify.notify_one();
                node.outgoing_file_acks
                    .lock()
                    .expect("outgoing file progress lock not poisoned")
                    .remove(&key);
                node.note_file_change();
            }
        }
        self.retries.complete(&key, completion.token, !failed, now);
        if failed {
            // Preserve only scalar candidate traversal outside active capacity.
            // Dropping the transfer closes any remaining observation-failure socket.
            if let Some(transfer) = transfers.remove(&key) {
                self.parked.insert(
                    key,
                    FileContinuation {
                        recipient_observation: transfer.recipient_observation,
                        routes: completion.routes,
                        candidate_cursor: transfer.candidate_cursor,
                        legacy: transfer.legacy,
                        sticker_proof_attempted: transfer.sticker_proof_attempted,
                        identity_offered: transfer.identity_offered,
                    },
                );
            }
            // Failure never manufactures an immediate wake.
            return;
        }
        if let Some(transfer) = transfers.remove(&key) {
            let kind = match transfer.phase {
                FilePhase::RelayHeld => transfer.held_observation.and_then(|mut observation| {
                    let generation = node.privacy.generation.load(Ordering::Acquire);
                    let changed = {
                        let roster = node.roster.lock().expect("roster lock not poisoned");
                        observation.refresh(&roster, now)
                    };
                    (!changed && observation.policy_generation == generation)
                        .then_some(FileHoldKind::RelayHeld(Box::new(observation)))
                }),
                FilePhase::ConsentWait => Some(FileHoldKind::ConsentWait),
                _ => {
                    self.transfers.insert(key, transfer);
                    node.delivery_notify.notify_one();
                    return;
                }
            };
            if let Some(kind) = kind {
                let delay = if matches!(kind, FileHoldKind::ConsentWait) {
                    FILE_CONSENT_RETRY
                } else {
                    FILE_RELAY_RETRY
                };
                if self.relay_cooldowns.len() == MAX_RELAY_CUSTODY_HINTS {
                    self.relay_cooldowns.pop_front();
                }
                self.relay_cooldowns.push_back(FileHold {
                    key,
                    until: now + delay,
                    kind,
                });
                #[cfg(test)]
                if matches!(
                    self.relay_cooldowns.back().unwrap().kind,
                    FileHoldKind::RelayHeld(_)
                ) {
                    node.trace_account_client(&node.identity.public(), || {
                        "file_relay_held_installed".into()
                    });
                }
                node.delivery_notify.notify_one();
            }
        }
    }
}

// Drives real protocol completions using the scheduler's monotonic clock seam.
#[cfg(test)]
pub(super) async fn assert_relay_held_repair(
    node: &Arc<Node>,
    lose_custody: impl FnOnce(),
    mut change_role: impl FnMut(bool),
) {
    let mut scheduler = FileDeliveryScheduler::default();
    let mut diagnostic = DeliveryDiagnosticThrottle::default();
    let mut roster_changes = node.subscribe_roster_changes();
    let mut raced = false;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while scheduler.relay_cooldowns.is_empty() {
            let scan = scheduler.scan(node);
            let (files, cursor) = scan.read(&node.delivery.lock().unwrap());
            scheduler.admit(node, files, cursor);
            scheduler.advance(node);
            if let Some(result) = scheduler.workers.join_next().await {
                let held = result.as_ref().is_ok_and(|(key, transfers, _)| {
                    transfers
                        .get(key)
                        .is_some_and(|transfer| matches!(transfer.phase, FilePhase::RelayHeld))
                });
                if held && !raced {
                    // Actual custody worker finished, but its result has not yet
                    // installed a hold. Consume the event with no hold present.
                    change_role(true);
                    roster_changes.changed().await.unwrap();
                    scheduler.refresh_held(node, std::time::Instant::now());
                    scheduler.finish(node, result, &mut diagnostic);
                    assert!(
                        scheduler.relay_cooldowns.is_empty(),
                        "worker baseline must retain a role event consumed before hold insertion"
                    );
                    change_role(false);
                    raced = true;
                } else {
                    scheduler.finish(node, result, &mut diagnostic);
                }
            } else {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
    })
    .await
    .expect("actual durable relay custody must finish");
    let key = scheduler.relay_cooldowns[0].key;
    let deadline = scheduler.relay_cooldowns[0].until;
    assert!(matches!(
        scheduler.relay_cooldowns[0].kind,
        FileHoldKind::RelayHeld(_)
    ));
    let before = deadline - FILE_RELAY_RETRY;
    for seconds in [0, 10, 60, 120, 299] {
        let now = before + std::time::Duration::from_secs(seconds);
        scheduler.refresh_held(node, now);
        let scan = scheduler.scan_at(node, now);
        let (files, cursor) = scan.read(&node.delivery.lock().unwrap());
        scheduler.admit_at(node, files, cursor, now);
        scheduler.advance_at(node, now);
        assert!(
            scheduler.workers.is_empty(),
            "quiet held key cannot own a worker"
        );
        assert!(scheduler.transfers.is_empty());
        assert_eq!(scheduler.relay_cooldowns[0].until, deadline);
    }
    lose_custody();
    let scan = scheduler.scan_at(node, deadline);
    assert!(scheduler.relay_cooldowns.is_empty());
    let (files, cursor) = scan.read(&node.delivery.lock().unwrap());
    scheduler.admit_at(node, files.clone(), cursor, deadline);
    scheduler.admit_at(node, files, cursor, deadline);
    scheduler.advance_at(node, deadline);
    assert_eq!(scheduler.workers.len(), 1, "one repair owner per exact key");
    let mut repair_now = deadline;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while scheduler.relay_cooldowns.is_empty() {
            if let Some(result) = scheduler.workers.join_next().await {
                scheduler.finish_at(node, result, &mut diagnostic, repair_now);
            }
            repair_now += std::time::Duration::from_secs(2);
            let scan = scheduler.scan_at(node, repair_now);
            let (files, cursor) = scan.read(&node.delivery.lock().unwrap());
            // A failed direct route retains ordinary failure pacing.
            let retry_now = repair_now;
            scheduler.admit_at(node, files, cursor, retry_now);
            scheduler.advance_at(node, retry_now);
            if scheduler.workers.is_empty() && scheduler.relay_cooldowns.is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
    })
    .await
    .expect("silent relay loss must repair through actual protocol work");
    assert_eq!(scheduler.relay_cooldowns[0].key, key);
    assert!(!node.outgoing_file_progress(key.0).unwrap().delivered);
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
        let next = fingerprints
            .into_iter()
            .map(|fingerprint| fingerprint.to_ascii_lowercase())
            .collect();
        {
            let mut current = self
                .verified_dm_pull_peers
                .lock()
                .expect("verified DM pull mutex not poisoned");
            if *current == next {
                return;
            }
            *current = next;
            self.delivery_demand_generation
                .fetch_add(1, Ordering::Release);
        }
        self.delivery_notify.notify_one();
        self.post_office_receive_notify.notify_one();
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
    /// Call only after durable acceptance of new work, never after an unchanged
    /// projection, successful poll or ordinary completion notification.
    pub(in crate::node) fn note_delivery_demand(&self) {
        self.delivery_demand_generation
            .fetch_add(1, Ordering::Release);
        self.delivery_notify.notify_one();
    }

    /// The delivery guard serializes this identity check with WAL acceptance.
    /// Retrying an already accepted pending intent must not create fresh demand.
    pub(in crate::node) fn begin_delivery_intent(
        &self,
        store: &mut DeliveryStore,
        transaction: DeliveryTransaction,
    ) -> Result<EventId, LogError> {
        let already_pending = store
            .pending_transactions()
            .iter()
            .any(|pending| pending.id() == transaction.id());
        use crate::transport::diagnostics;
        let tracks_message = matches!(
            transaction.body(),
            DeliveryTransaction::Outgoing { .. }
                | DeliveryTransaction::OutgoingManifest { .. }
                | DeliveryTransaction::ChannelOutgoing { .. }
        );
        let result = store.begin(transaction);
        if tracks_message {
            diagnostics::record_message(|metrics| match &result {
                Ok(_) if !already_pending => metrics.accepted = metrics.accepted.saturating_add(1),
                Err(_) => metrics.rejected = metrics.rejected.saturating_add(1),
                _ => {}
            });
        }
        let id = result?;
        if !already_pending {
            self.note_delivery_demand();
        }
        Ok(id)
    }

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
        self.begin_delivery_intent(store, transaction)
            .map_err(NodeError::Log)?;
        let recovered = match self.recover_delivery(store) {
            Ok(()) => true,
            Err(error) => {
                log::warn!("accepted outgoing delivery awaits local recovery: {error}");
                false
            }
        };
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
        if !self.personal_peer_allowed(&peer, Some(proof)) {
            return Err(LogError::AdmissionRejected(
                "personal communication denied".into(),
            ));
        }
        if self.files.lock().expect("files lock").processed(event.id)? {
            crate::transport::diagnostics::note_duplicate_receive();
            return Ok(());
        }
        let manifest = super::files::validated_manifest(&received.plaintext)
            .ok_or_else(|| LogError::CorruptFile("invalid file manifest".into()))?;
        let logical = self.file_message_row_logical_id(&received)?;
        if self
            .account_suppression
            .lock()
            .expect("account suppression lock")
            .contains(received.conversation, logical)?
        {
            self.files
                .lock()
                .expect("files lock")
                .mark_processed(event.id)?;
            return Ok(());
        }
        if store.manifest_event_erased(event.id)
            || manifest.file_conv() == event.conversation_id
            || manifest.file_conv() == received.conversation
            || !self
                .files
                .lock()
                .expect("files lock not poisoned")
                .accepts(&manifest)?
        {
            return Err(LogError::CorruptFile(
                "conflicting immutable file manifest".into(),
            ));
        }
        {
            let file_book = self.files.lock().expect("files lock").clone();
            let log = self.log.lock().expect("log lock not poisoned");
            let first = log
                .headers_page(
                    &manifest.file_conv(),
                    None,
                    crate::eventlog::repository::pages::PageBudget {
                        rows: 1,
                        bytes: std::mem::size_of::<crate::eventlog::repository::EventHeader>(),
                    },
                )?
                .items
                .into_iter()
                .next();
            if let Some(first) = first {
                let count = log.repository()?.count(&manifest.file_conv())?;
                if count > u64::from(manifest.chunk_count())
                    || (first.author != event.author
                        && !file_book.has_author(manifest.file_conv(), first.author)?)
                {
                    return Err(LogError::CorruptFile(
                        "conflicting file chunk conversation".into(),
                    ));
                }
                let mut chain = super::files::ChunkChain::default();
                // The former collector sorted by sequence. Exact author/sequence
                // seeks retain that rule, and missing or mixed-author slots fail.
                for index in 0..count {
                    let chunk = log
                        .repository()?
                        .authored_file_chunk(
                            &manifest.file_conv(),
                            first.author,
                            index as u32,
                            4 * 1024 * 1024,
                        )?
                        .ok_or_else(|| {
                            LogError::CorruptFile("conflicting file chunk conversation".into())
                        })?;
                    if !chain.accept(&chunk)
                        || crate::file::open_chunk_for(&manifest, index as u32, &chunk.ciphertext)
                            .is_err()
                    {
                        return Err(LogError::CorruptFile(
                            "conflicting file chunk conversation".into(),
                        ));
                    }
                }
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
        self.begin_delivery_intent(
            &mut store,
            DeliveryTransaction::IncomingManifest {
                sender: peer,
                original: Box::new(event.clone()),
                received: Box::new(received),
                receipt,
                file,
            },
        )?;
        self.recover_delivery(&mut store)?;
        Ok(())
    }
    pub(in crate::node) fn persist_account_adoption<T>(
        &self,
        account: &crate::identity::account::AccountPublic,
        save: impl FnOnce() -> Result<T, NodeError>,
        reload: impl FnOnce() -> Result<crate::identity::account::AccountPublic, NodeError>,
    ) -> Result<T, NodeError> {
        let mut store = self.delivery.lock().expect("delivery lock not poisoned");
        if self
            .delivery_suspended
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(NodeError::Channel(
                "account adoption requires runtime restart".into(),
            ));
        }
        if account == &self.account.public() {
            return save();
        }
        self.recover_delivery(&mut store).map_err(NodeError::Log)?;
        store
            .prepare_profile_adoption(&account.account_id())
            .map_err(NodeError::Log)?;
        let database = self.storage.participant_database().ok_or_else(|| {
            NodeError::Log(LogError::CorruptFile(
                "participant bootstrap has not completed".into(),
            ))
        })?;
        if let Err(error) = crate::storage::profile::participant::prepare_adoption(
            database,
            &self.account.public(),
            account,
        ) {
            self.delivery_suspended
                .store(true, std::sync::atomic::Ordering::Release);
            return Err(NodeError::Log(error));
        }
        match save() {
            Ok(result) => {
                self.delivery_suspended
                    .store(true, std::sync::atomic::Ordering::Release);
                self.account_sync_notify.notify_one();
                Ok(result)
            }
            Err(error) => {
                // Durable replacement may have happened before a sync/save error.
                // Only an authenticated reload of the exact old key permits cancel.
                match reload() {
                    Ok(loaded) if loaded == self.account.public() => {
                        let cleanup = store
                            .cancel_profile_adoption()
                            .and_then(|()| {
                                self.account_transitions
                                    .lock()
                                    .expect("account transition lock not poisoned")
                                    .discard_pending(&self.identity.public(), &loaded.ed25519_pub)
                            })
                            .and_then(|()| {
                                crate::storage::profile::participant::finish_adoption(
                                    database, &loaded,
                                )
                            });
                        if let Err(cleanup) = cleanup {
                            self.delivery_suspended
                                .store(true, std::sync::atomic::Ordering::Release);
                            return Err(NodeError::Channel(format!(
                                "{error}; account adoption cleanup: {cleanup}"
                            )));
                        }
                    }
                    Ok(loaded) => {
                        self.delivery_suspended
                            .store(true, std::sync::atomic::Ordering::Release);
                        if &loaded != account {
                            return Err(NodeError::Channel(format!(
                                "{error}; account keystore has an unrelated identity"
                            )));
                        }
                        return Err(NodeError::Channel(format!(
                            "{error}; saved account changed; restart runtime to recover adoption"
                        )));
                    }
                    Err(reload) => {
                        self.delivery_suspended
                            .store(true, std::sync::atomic::Ordering::Release);
                        return Err(NodeError::Channel(format!(
                            "{error}; reload account keystore: {reload}"
                        )));
                    }
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
    ) -> Result<Option<()>, LogError> {
        self.accept_dm_event_with_author(event, &proof.public(), Some(proof), store)
    }

    pub(in crate::node) fn accept_dm_event_with_author(
        &self,
        event: &Event,
        public: &PublicIdentity,
        proof: Option<&crate::discovery::Announce>,
        store: &mut DeliveryStore,
    ) -> Result<Option<()>, LogError> {
        let peer = public.clone();
        let sibling = self.certified_sibling(&peer);
        // Offline replay can use the already persisted exact sibling certificate.
        // Receipt and control processing still require the real signed announcement.
        if proof.is_none() && !sibling {
            return Ok(None);
        }
        let own = self.identity.public();
        if event.conversation_id == super::delivery_receipt::delivery_conversation_id(&own, &peer) {
            if let Some(proof) = proof {
                self.process_delivery_control(event, proof, store);
            }
            return Ok(None);
        }
        if event.conversation_id != super::conversation::dm_conversation_id(&own, &peer)
            || event.author.ed25519_pub() != &peer.ed25519_pub
            || !event.verify_integrity()
            || !event.verify_signature()
            || event.ciphertext.len() > 256 * 1024
            || event.parents.len() > 1024
        {
            return Ok(None);
        }
        if self
            .received
            .lock()
            .expect("received mutex")
            .processed(event.id)?
        {
            crate::transport::diagnostics::note_duplicate_receive();
            return Ok(Some(()));
        }
        if !sibling && !self.personal_peer_allowed(&peer, proof) {
            return Ok(None);
        }
        if sibling && !self.device_sync_policy().history {
            return Ok(None);
        }
        if !sibling && let Some(peer_account) = proof.and_then(|proof| proof.account_id()) {
            self.prepare_live_personal_dm(&peer_account, store)
                .map_err(delivery_log_error)?;
        }
        store.check_capacity(1, event.ciphertext.len() as u64)?;
        let decrypted = self
            .dm_ratchet
            .lock()
            .expect("ratchet lock not poisoned")
            .prepare_decrypt(&self.identity, &peer, &event.ciphertext);
        let Ok((plain, prepared)) = decrypted else {
            return Ok(None);
        };
        if plain.len() > 256 * 1024 {
            return Ok(None);
        }
        let account = self.account_id();
        let sender_account = match proof {
            Some(proof) => proof.account_id(),
            None => Some(account.clone()),
        };
        let (conversation, from) = if plain.starts_with(b"MTDE1") {
            let Some(env) = DmEnvelope::decode(&plain) else {
                return Ok(None);
            };
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
                return Ok(None);
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
            let Some(proof) = proof else {
                return Ok(None);
            };
            if store.has_receipt_for(conversation, event.id) {
                return Ok(None);
            }
            let Ok(wire) = payload.seal(&self.identity) else {
                return Ok(None);
            };
            let log = self.log.lock().expect("log lock not poisoned");
            let conv = payload.conversation();
            let (mut parents, lamport) = log.prepare(&conv)?;
            // The Ed-pair scope survives account adoption and local deletion.
            // Never make a fresh receipt depend on control ancestry that the
            // current profile cannot safely project to this exact account.
            let controls = self
                .delivery_control_ids
                .lock()
                .expect("control ids lock not poisoned");
            let mut eligible = std::collections::HashSet::new();
            for previous in log.events(&conv)? {
                let typed = (previous.author.ed25519_pub() == &own.ed25519_pub
                    && controls
                        .get(&previous.id)
                        .is_some_and(|b| b.device == peer && b.account == proof.account_id()))
                    || super::delivery_receipt::open_receipt(
                        &self.identity,
                        &account,
                        proof,
                        &previous,
                    )
                    .is_some();
                if typed && previous.parents.iter().all(|id| eligible.contains(id)) {
                    eligible.insert(previous.id);
                }
            }
            parents.retain(|id| eligible.contains(id));
            drop(controls);
            let author = crate::eventlog::Author::from_ed25519(own.ed25519_pub);
            let seq = log.next_sequence(&conv, &author)?;
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
            log.sync()?;
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
        self.log.lock().expect("log lock not poisoned").sync()?;
        // Hold policy through durable acceptance; accepted WAL recovery remains unconditional.
        let policy = self
            .account_sync_policy
            .lock()
            .expect("account sync policy mutex not poisoned");
        if sibling && !policy.current().history {
            return Ok(None);
        }
        if !sibling && let Some(peer_account) = proof.and_then(|proof| proof.account_id()) {
            self.reopen_personal_after_live(&peer_account)
                .map_err(delivery_log_error)?;
        }
        self.begin_delivery_intent(
            store,
            DeliveryTransaction::Incoming {
                sender: peer.clone(),
                original: Box::new(event.clone()),
                ratchet: prepared,
                received: Box::new(received),
                receipt,
            },
        )?;
        drop(policy);
        self.recover_delivery(store)?;
        Ok(Some(()))
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
        // Recovery may retire several bounded WAL intents. Their canonical
        // refreshes share one allowance instead of resetting it per channel.
        let mut control_budget = crate::eventlog::sync::network::PreparationBudget::default();
        loop {
            #[cfg(test)]
            let recovery_operation = store
                .pending_transactions()
                .first()
                .and_then(|transaction| {
                    transaction.group_operation().map(|reference| reference.id)
                });
            #[cfg(test)]
            let recovery_trace = recovery_operation.and_then(|id| {
                let enabled = self.account_client_test_hook.lock().unwrap().is_some();
                enabled.then(|| (id, self.identity.public(), std::time::Instant::now()))
            });
            #[cfg(test)]
            let recovery_phase = |phase: &str| {
                if let Some((id, peer, clock)) = &recovery_trace {
                    self.trace_account_client(peer, || {
                        format!(
                            "operation_owner recovery_phase={phase} id={id:?} elapsed_ms={}",
                            clock.elapsed().as_millis()
                        )
                    });
                }
            };
            let notification =
                store
                    .pending_transactions()
                    .first()
                    .and_then(|tx| match tx.body() {
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
                        DeliveryTransaction::ChannelOutgoing { events, sent, .. }
                            if events.last().is_some_and(|event| {
                                event.kind == crate::eventlog::EventKind::FileManifest
                            }) =>
                        {
                            Some((
                                self.identity.public(),
                                sent.conversation,
                                super::received_log::ReceivedEntry {
                                    event_id: tx.id(),
                                    conversation: sent.conversation,
                                    from: self.user_id(),
                                    wall_clock: sent.wall_clock,
                                    plaintext: sent.plaintext.clone(),
                                },
                                true,
                                false,
                            ))
                        }
                        _ => None,
                    });
            let prepared_file_scopes =
                match store.pending_transactions().first().map(|tx| tx.body()) {
                    Some(DeliveryTransaction::OutgoingManifest {
                        message,
                        received,
                        file,
                    }) => self.prepare_manifest_transaction_scopes(message, received, file)?,
                    _ => Vec::new(),
                };
            // Channel manifests already carry their complete plaintext in the WAL.
            // Retain their signed chunk scope before retiring that transaction; a
            // failed scope/sidecar write remains recoverable after acceptance.
            if let Some((_, channel, entry, true, false)) = &notification
                && matches!(
                    store.pending_transactions().first().map(|tx| tx.body()),
                    Some(DeliveryTransaction::ChannelOutgoing { .. })
                )
                && let Some(manifest) = super::files::validated_manifest(&entry.plaintext)
            {
                self.remember_sent_manifest_scope(
                    manifest.file_conv(),
                    *channel,
                    crate::eventlog::Author::from_ed25519(self.identity.public().ed25519_pub),
                    entry.event_id,
                )?;
            }
            let wake_local =
                store
                    .pending_transactions()
                    .first()
                    .is_some_and(|tx| match tx.body() {
                        DeliveryTransaction::Outgoing { .. }
                        | DeliveryTransaction::OutgoingManifest { .. }
                        | DeliveryTransaction::ChannelOutgoing { .. }
                        | DeliveryTransaction::ChannelControl { .. }
                        | DeliveryTransaction::AccountSelfControl { .. }
                        | DeliveryTransaction::LegacyMemberLeave { .. }
                        | DeliveryTransaction::AccountOwnerControl { .. }
                        | DeliveryTransaction::ChannelMessageAction { .. } => true,
                        DeliveryTransaction::Incoming { sender, .. }
                        | DeliveryTransaction::IncomingManifest { sender, .. } => {
                            !self.certified_sibling(sender)
                        }
                        DeliveryTransaction::Requested { .. }
                        | DeliveryTransaction::GroupRequested { .. }
                        | DeliveryTransaction::AccountGroupFile { .. }
                        | DeliveryTransaction::AccountInvitation { .. } => false,
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
            let control_channel =
                store
                    .pending_transactions()
                    .first()
                    .and_then(|tx| match tx.body() {
                        DeliveryTransaction::ChannelControl { events }
                        | DeliveryTransaction::AccountSelfControl { events, .. }
                        | DeliveryTransaction::LegacyMemberLeave { events, .. }
                        | DeliveryTransaction::AccountOwnerControl { events, .. } => {
                            events.first().map(|event| event.conversation_id)
                        }
                        _ => None,
                    });
            #[cfg(test)]
            recovery_phase("guards_begin");
            let installed = {
                let mut ratchet = self.dm_ratchet.lock().expect("ratchet lock not poisoned");
                let mut log = self.log.lock().expect("log lock not poisoned");
                if let Some(tx) = store.pending_transactions().first() {
                    store.validate_message_action_transaction(tx, &log, &self.identity)?;
                }
                #[cfg(test)]
                {
                    let hook = self.before_recovery_sent_hook.lock().unwrap().take();
                    if let Some(hook) = hook {
                        hook();
                    }
                }
                let mut sent = self.sentlog.lock().expect("sent lock not poisoned");
                let mut received = self.received.lock().expect("received lock not poisoned");
                let mut files = self.received_files.lock().expect("files lock not poisoned");
                let recall = store
                    .pending_transactions()
                    .first()
                    .and_then(|tx| match tx.body() {
                        DeliveryTransaction::ChannelMessageAction { recall, .. } => recall.clone(),
                        _ => None,
                    });
                let mut complete =
                    |operation: &super::account_group::operations::journal::OperationReference,
                     events: &[Event]| {
                        // Startup and running recovery use the same binding and
                        // durable sidecar rule before completing the original ID.
                        #[cfg(test)]
                        recovery_phase("completion_begin");
                        super::account_group::operations::persist_action_recall_completion(
                            operation,
                            recall.as_ref(),
                            &mut self.recalls.lock().expect("recalls lock"),
                        )?;
                        let result = self.complete_account_group_operation(operation, events);
                        #[cfg(test)]
                        recovery_phase("completion_returned");
                        result
                    };
                #[cfg(test)]
                recovery_phase("replay_begin");
                store.recover_next_with_group_operations(
                    &mut ratchet,
                    &mut log,
                    &mut *sent,
                    &mut *received,
                    Some(&mut *files),
                    Some(&mut self.channel_senders.lock().expect("sender store lock")),
                    Some(&mut complete),
                )
            };
            #[cfg(test)]
            recovery_phase("replay_returned");
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
                    if epoch != state.epoch() {
                        continue;
                    }
                    if let Some(key) = crate::channel::sender_key::SenderKey::deserialize(&bytes) {
                        state.import_my_sender(epoch, key);
                    }
                }
            }
            if removed {
                self.install_file_card_scopes(prepared_file_scopes);
            }
            if removed && let Some(channel) = control_channel {
                // Finish short control tails after releasing projection guards.
                // A larger tail keeps its existing runtime ticket without
                // turning an installed effect into failed durable recovery.
                // Terminal status never permits reading a stale projection.
                #[cfg(test)]
                recovery_phase("projection_begin");
                match self.prepare_channel_admission(channel, &mut control_budget) {
                    Ok(()) | Err(LogError::StaleCursor) => {}
                    Err(error) => return Err(error),
                }
                #[cfg(test)]
                recovery_phase("projection_returned");
                self.account_sync_notify.notify_one();
            }
            if removed && wake_local {
                self.note_account_local_change();
            }
            if removed && let Some((sender, original_conv, entry, file, incoming)) = notification {
                self.note_account_history_change();
                if file {
                    if incoming {
                        self.note_reception_change();
                    }
                    if let Some(manifest) = super::files::validated_manifest(&entry.plaintext) {
                        self.remember_manifest_scope(
                            manifest.file_conv(),
                            original_conv,
                            crate::eventlog::Author::from_ed25519(sender.ed25519_pub),
                            entry.event_id,
                        );
                        if incoming
                            && !self.recipient_expired(
                                &entry.plaintext,
                                entry.event_id,
                                entry.wall_clock,
                            )
                        {
                            self.file_pull_notify.notify_one();
                            if !self.certified_sibling(&sender) {
                                let logical = self.file_message_row_logical_id(&entry)?;
                                let aliases = self
                                    .file_message_identities
                                    .lock()
                                    .expect("file identities lock")
                                    .aliases(entry.conversation, logical)?;
                                let _ = self.file_incoming.send(ReceivedFile {
                                    received_via: self.reception_effects.received_file_via(
                                        entry.conversation,
                                        entry.event_id,
                                        &aliases,
                                    )?,
                                    protection: manifest.protection(),
                                    expires_at_ms: crate::expiry::deadline(&entry.plaintext),
                                    conv: original_conv,
                                    is_channel: false,
                                    from: sender.user_id(),
                                    name: if manifest.protection().is_some() {
                                        "Protected image".into()
                                    } else {
                                        manifest.name().to_owned()
                                    },
                                    size: manifest.size(),
                                    mime: manifest.mime().to_owned(),
                                    file_conv: manifest.file_conv(),
                                    media: super::media_store::manifest_is_media(&manifest),
                                    event_id: logical,
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
                } else {
                    self.note_reception_change();
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
                            received_via: self.reception_effects.received_via(
                                entry.conversation,
                                envelope
                                    .as_ref()
                                    .map_or(entry.event_id, |env| EventId::new(env.msg_id)),
                            )?,
                            message_id: envelope.as_ref().map(|env| EventId::new(env.msg_id)),
                            from_account: envelope
                                .as_ref()
                                .map(|env| env.route.sender_account.clone()),
                            from: sender.user_id(),
                            from_name,
                            text: body.text,
                            protection: body.protection,
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

    /// Read existing direct outbox state without waiting on an active writer.
    pub fn diagnostic_delivery_backlog(&self) -> Option<(usize, Option<u64>)> {
        let (count, oldest) = self.delivery.try_lock().ok()?.diagnostic_backlog();
        Some((
            count,
            oldest.and_then(|time| super::node::now_millis().checked_sub(time)),
        ))
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
        let mut consent = None;
        for destination in recipients.iter().filter(|destination| destination.active) {
            match self.outgoing_file_consent_status(id, destination.binding.event_id) {
                Ok(Some(crate::eventlog::sync::FileConsentStatus::Declined)) => {
                    consent = Some(super::ReceiveDecision::Declined);
                    break;
                }
                Ok(Some(crate::eventlog::sync::FileConsentStatus::Pending)) => {
                    consent = Some(super::ReceiveDecision::Pending)
                }
                Ok(_) => {}
                Err(error) => {
                    log::warn!("file progress consent observation failed: {error}");
                    return None;
                }
            }
        }
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

    pub(in crate::node) fn sent_header_identity(
        &self,
        header: &super::transcript_repository::TranscriptHeader,
    ) -> Result<EventId, LogError> {
        if let Some(id) = self
            .recovered_sent_ids
            .lock()
            .expect("recovered identity mutex")
            .as_ref()
            .map(|r| r.id_for_header(header))
            .transpose()?
            .flatten()
        {
            return Ok(id);
        }
        let author = crate::eventlog::Author::from_ed25519(self.identity.public().ed25519_pub);
        Ok(self
            .log
            .lock()
            .expect("log mutex")
            .repository()?
            .authored_header(&header.conversation, author, header.sequence)?
            .filter(|h| h.kind == crate::eventlog::EventKind::Message)
            .map(|h| h.id)
            .unwrap_or(EventId::new([0; 32])))
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
        #[cfg(test)]
        self.trace_account_client(&proof.public(), || "delivery_receipt_open".into());
        let Some(opened) =
            super::delivery_receipt::open_receipt(&self.identity, &self.account_id(), proof, event)
        else {
            return;
        };
        if store
            .completed_delivery(opened.logical_id())
            .is_some_and(|completed| opened.irrelevant_to_completed(completed))
        {
            // The exact delivery guard also serializes cancel/edit. A fully
            // opened but now irrelevant control retires only receive work.
            if self
                .log
                .lock()
                .expect("log lock not poisoned")
                .append_durable(event.clone())
                .is_ok()
            {
                self.retire_retained_delivery_control(event.id);
            }
            return;
        }
        let canonical = store
            .logical_for_original(opened.original_event_id())
            .unwrap_or_else(|| opened.logical_id());
        let message = match self.immutable_delivery(store, canonical) {
            Ok(message) => message,
            Err(error) => {
                log::warn!("delivery receipt lookup deferred: {error}");
                return;
            }
        };
        let Some(message) = message else {
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
        let first_delivery =
            store.status(authenticated.logical_id()) == Some(DeliveryStatus::Awaiting);
        if store
            .mark_delivered_for(
                authenticated.logical_id(),
                authenticated.original_event_id(),
            )
            .is_ok()
        {
            if first_delivery {
                crate::transport::diagnostics::record_message(|metrics| {
                    metrics.delivered(super::node::now_millis().checked_sub(message.wall_clock));
                });
            }
            self.retire_retained_delivery_control(event.id);
            self.delivery_observations
                .lock()
                .expect("delivery observations lock not poisoned")
                .retain(|logical, _| logical != authenticated.logical_id());
            self.delivery_status_notify.notify_waiters();
        }
    }

    /// Call only after the exact signed control is durably retained and its
    /// handling has reached a terminal result under the delivery guard.
    fn retire_retained_delivery_control(&self, id: EventId) {
        let result = self
            .log
            .lock()
            .expect("log lock not poisoned")
            .repository()
            .and_then(|repository| repository.complete_control(id));
        // Failed retirement stays pending. Completed irrelevance permits safe
        // later recovery even after the original destination was retired.
        if let Err(error) = result {
            log::warn!("delivery receipt work retirement deferred: {error}");
        }
    }

    fn immutable_delivery(
        &self,
        store: &DeliveryStore,
        id: EventId,
    ) -> Result<Option<OutgoingDelivery>, crate::eventlog::LogError> {
        if let Some(message) = store.message(id) {
            return Ok(Some(message.clone()));
        }
        let Some(completed) = store.completed_work(id) else {
            return Ok(None);
        };
        let log = self.log.lock().expect("log lock not poisoned");
        let mut destinations = Vec::with_capacity(completed.remaining.len());
        for reference in &completed.remaining {
            let Some(event) = log.get(&reference.event_id)? else {
                return Ok(None);
            };
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
                return Ok(None);
            }
            destinations.push(DeliveryDestination {
                device: reference.device.clone(),
                account: reference.account.clone(),
                event,
                receipt_eligible: reference.receipt_eligible,
            });
        }
        Ok(Some(OutgoingDelivery {
            logical_id: id,
            sender_account: self.account_id(),
            recipient_account: completed.recipient_account.clone(),
            conversation: completed.conversation,
            wall_clock: completed.wall_clock,
            destinations,
        }))
    }

    /// Immediate best-effort transfer of an already accepted immutable intent.
    /// Direct and relay alternatives share the recovery worker's exchange rules.
    /// One network deadline covers the full fanout; expiry leaves durable work
    /// pending. A transport success is never a delivery confirmation.
    pub async fn flush_delivery(self: &Arc<Self>, id: EventId) {
        let message = {
            let mut store = self.delivery.lock().expect("delivery lock not poisoned");
            if let Err(error) = self.recover_delivery(&mut store) {
                log::warn!("delivery recovery deferred: {error}");
                return;
            }
            match self.immutable_delivery(&store, id) {
                Ok(message) => message,
                Err(error) => {
                    log::warn!("delivery flush lookup deferred: {error}");
                    return;
                }
            }
        };
        let Some(message) = message else {
            return;
        };
        let transfer =
            async {
                for destination in message.destinations {
                    Box::pin(self.retry_one_destination(
                        &destination,
                        DeliveryAttemptContext::ImmediateFlush,
                    ))
                    .await;
                }
            };
        let _ = tokio::time::timeout(super::route_timing::MAX_CONTROL_DEADLINE, transfer).await;
    }

    /// Bounded direct receipt transfer hook. The shared worker separately
    /// qualifies exact durable custody and retires queued work.
    pub async fn flush_delivery_receipts(self: &Arc<Self>, limit: usize) {
        let receipts = {
            let mut store = self.delivery.lock().expect("delivery lock not poisoned");
            if let Err(error) = self.recover_delivery(&mut store) {
                log::warn!("delivery recovery deferred: {error}");
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
        self: &Arc<Self>,
        peer: &crate::discovery::PeerRecord,
        destination: &DeliveryDestination,
        conversation: ConversationId,
    ) -> bool {
        self.transfer_delivery_destination_with_admission(
            peer,
            destination,
            conversation,
            super::session_pool::AuthenticationAdmission::Backpressure,
            None,
        )
        .await
        .0 == TransferOutcome::Held
    }

    async fn transfer_delivery_destination_with_admission(
        self: &Arc<Self>,
        peer: &crate::discovery::PeerRecord,
        destination: &DeliveryDestination,
        conversation: ConversationId,
        admission: super::session_pool::AuthenticationAdmission,
        receipt_ready: Option<&tokio::sync::Notify>,
    ) -> (TransferOutcome, bool) {
        for fresh in [false, true] {
            let started = std::time::Instant::now();
            let dial = if fresh {
                self.privacy_dial_with_admission(peer.addr, &destination.device, admission)
                    .await
                    .map(|channel| (channel, false))
            } else {
                self.privacy_sync_dial_with_admission(peer.addr, &destination.device, admission)
                    .await
            };
            let Ok((mut channel, reused)) = dial else {
                #[cfg(test)]
                if let Err(error) = &dial {
                    self.trace_account_client(&peer.public, || {
                        format!("delivery_range_failed stage=dial error={error}")
                    });
                }
                return (TransferOutcome::Failed, false);
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
                return (TransferOutcome::Failed, false);
            }
            let exchange = async {
                let store = self.sync_store(channel.peer_identity());
                // A response can accept a reverse-direction DM before this
                // outgoing attempt is cancelled or its durable-have probe ends.
                // Its actual reception owner must outlive the awaiting caller.
                let _reception = self
                    .request_reception_scope(&store, &channel)
                    .ok_or(SessionError::UnexpectedMessage)?;
                let result =
                    super::session::request_sync_turn_paged(&mut channel, &store, conversation)
                        .await;
                self.dispatch_received_sync_work(&store);
                #[cfg(test)]
                self.trace_account_client(&peer.public, || match &result {
                    Ok(outcome) => format!(
                        "delivery_range_done message={} complete={} limited={} applied={}",
                        conversation == destination.event.conversation_id,
                        outcome.complete,
                        outcome.limited,
                        outcome.applied
                    ),
                    Err(error) => format!("delivery_range_failed stage=sync error={error}"),
                });
                let outcome = result?;
                // This framed range turn has actually completed. Publish its
                // measured cost before the separate Have RPC, so the existing
                // adaptive owner can use its existing deadline ceiling in time.
                self.observe_control_exchange(&channel, started);
                let (complete, limited) = (outcome.complete, outcome.limited);
                if conversation != destination.event.conversation_id {
                    return Ok((false, complete, limited));
                }
                super::session::request_durable_have(
                    &mut channel,
                    conversation,
                    destination.event.id,
                )
                .await
                .map(|held| (held, complete, limited))
            }
            .await;
            match exchange {
                Ok((held, complete, limited)) => {
                    #[cfg(test)]
                    self.trace_account_client(&peer.public, || {
                        format!(
                            "delivery_range_have message={} held={held} complete={complete} limited={limited}",
                            conversation == destination.event.conversation_id
                        )
                    });
                    self.observe_control_exchange(&channel, started);
                    let held = held && self.delivery_destination_allowed(destination);
                    if let Some(ready) = receipt_ready.filter(|_| held) {
                        // Finish original reception promptly even if the early
                        // control round is still in flight.
                        self.emit_new_messages(conversation);
                        ready.notified().await;
                    }
                    let follow_receipt = held
                        && receipt_ready.is_some()
                        && self.delivery_destination_allowed(destination)
                        && self
                            .delivery
                            .lock()
                            .expect("delivery lock not poisoned")
                            .contains_work_event(destination.event.id);
                    if follow_receipt {
                        // Keep the exact authenticated stream after positive Have.
                        // The enclosing attempt still owns its original deadline.
                        let control = super::delivery_receipt::delivery_conversation_id(
                            &self.identity.public(),
                            &destination.device,
                        );
                        let store = self.sync_store(channel.peer_identity());
                        let Some(_reception) = self.request_reception_scope(&store, &channel)
                        else {
                            return (TransferOutcome::Held, false);
                        };
                        let result =
                            super::session::request_sync_turn_paged(&mut channel, &store, control)
                                .await;
                        self.dispatch_received_sync_work(&store);
                        self.emit_new_messages(control);
                        if result.is_err() {
                            return (TransferOutcome::Held, false);
                        }
                    }
                    if held || !complete || conversation != destination.event.conversation_id {
                        self.recycle_sync_channel(channel);
                    }
                    let outcome = if !self.delivery_destination_allowed(destination) {
                        TransferOutcome::Failed
                    } else if held {
                        TransferOutcome::Held
                    } else if limited {
                        TransferOutcome::Limited
                    } else if !complete {
                        TransferOutcome::More
                    } else {
                        TransferOutcome::Failed
                    };
                    return (outcome, follow_receipt);
                }
                Err(error) if reused && super::session_pool::retryable_sync_error(&error) => {}
                Err(_) => return (TransferOutcome::Failed, false),
            }
        }
        (TransferOutcome::Failed, false)
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
        let generation = self
            .privacy
            .generation
            .load(std::sync::atomic::Ordering::SeqCst);
        let attempts = futures_util::stream::iter(candidates)
            .map(|po| async move {
                if po.public == self.identity.public()
                    || !self.relay_send_allowed(&po.public)
                    || !self.relay_retry_due(&po.public)
                {
                    return false;
                }
                let result = self
                    .adaptive_control_timeout(&po.public, async {
                        for fresh in [false, true] {
                            let started = std::time::Instant::now();
                            let dial = if fresh {
                                self.privacy_dial_with_admission(
                                    po.addr,
                                    &po.public,
                                    super::session_pool::AuthenticationAdmission::RouteProgress,
                                )
                                .await
                                .map(|channel| (channel, false))
                            } else {
                                self.privacy_delivery_sync_dial_with_reuse(po.addr, &po.public)
                                    .await
                            };
                            let Ok((mut channel, reused)) = dial else {
                                return Ok(false);
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
                                return Ok(false);
                            }
                            let exchange = self
                                .confirm_history_relay_custody(
                                    &mut channel,
                                    &destination.device,
                                    destination.event.conversation_id,
                                    destination.event.id,
                                )
                                .await;
                            match exchange {
                                Ok(held) => {
                                    self.observe_control_exchange(&channel, started);
                                    if held {
                                        self.recycle_sync_channel(channel);
                                    }
                                    return Ok(held
                                        && self.delivery_destination_allowed(destination)
                                        && self.relay_send_allowed(&po.public));
                                }
                                Err(error)
                                    if reused
                                        && super::session_pool::retryable_sync_error(&error) => {}
                                Err(error) => return Err(error),
                            }
                        }
                        Ok(false)
                    })
                    .await
                    .unwrap_or(Ok(false));
                if matches!(result, Ok(true)) {
                    self.observe_exact_relay_custody(destination, &po.public, generation);
                }
                self.record_relay_attempt_result(&po.public, result.as_ref().copied())
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
        // Always retain the current best candidate; rotate alternatives so cold
        // and recovered paths are sampled without delaying a healthy fast route.
        let alternatives = candidates.len().saturating_sub(1);
        let offset = if alternatives == 0 {
            0
        } else {
            cursors.get(scope).copied().unwrap_or(0) % alternatives
        };
        candidates[1..].rotate_left(offset);
        let next = offset.wrapping_add(alternatives.min(3));
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

    /// A local pause preserves the relay's current health and leaves retry
    /// pacing to the operation that owns the interrupted exchange.
    pub(super) fn record_relay_attempt_result(
        &self,
        relay: &PublicIdentity,
        result: Result<bool, &SessionError>,
    ) -> bool {
        if matches!(
            result,
            Err(SessionError::ResourceUnavailable | SessionError::Store(LogError::StaleCursor))
        ) {
            return false;
        }
        let success = result.unwrap_or(false);
        self.record_relay_attempt(relay, success);
        success
    }

    fn control_routes(&self, peer: &PublicIdentity) -> ControlRoutes {
        let roster = self.roster.lock().expect("roster mutex not poisoned");
        let mut relays = std::collections::BTreeSet::new();
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
                relays.insert(RelayRouteInput::observe(&relay));
            }
        }
        ControlRoutes {
            peer: peer.clone(),
            endpoints,
            relays,
            publicly_observed,
            policy_generation: self.privacy.generation.load(Ordering::Acquire),
            demand_generation: self.delivery_demand_generation.load(Ordering::Acquire),
        }
    }

    fn control_budget(&self, peer: &PublicIdentity) -> std::time::Duration {
        let observed = self
            .routing_peer(&peer.user_id())
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
            });
        let preferred = self
            .preferred_routes
            .lock()
            .expect("routes lock not poisoned")
            .get(&peer.user_id())
            .copied();
        preferred.map_or(observed, |address| {
            observed.max(self.sessions.control_timeout(peer, address))
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

    fn cancel_changed_delivery_routes(
        &self,
        active: &mut std::collections::HashMap<ControlWork, ControlRoutes>,
        owners: &mut ControlTaskOwners,
    ) {
        let changed: Vec<_> = active
            .iter()
            .filter(|(_, routes)| {
                let current = self.control_routes(&routes.peer);
                current.endpoints != routes.endpoints || current.relays != routes.relays
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in changed {
            owners.cancel(&key);
            active.remove(&key);
            // A completed-but-unjoined old task may have moved its observation
            // token out of its Drop guard. Remove only that exact destination.
            if let ControlWork::Destination(event) = key {
                crate::transport::diagnostics::record_message(|metrics| {
                    metrics.route_restarts = metrics.route_restarts.saturating_add(1);
                });
                self.delivery_observations
                    .lock()
                    .expect("delivery observations lock not poisoned")
                    .retain(|_, destination| destination != event);
                self.delivery_status_notify.notify_waiters();
            }
        }
    }

    /// Shared by desktop, CLI and SDK accept-loop hosts. The future owns all
    /// cursors and network work, so dropping it cancels scheduling and dials.
    pub(in crate::node) async fn run_delivery_loop(self: std::sync::Arc<Self>) {
        use std::time::Duration;
        let _observation_scope = ObservationScope(self.clone());
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut idle_repair = None;
        let mut destination_cursor = None;
        let mut receipt_cursor = None;
        let mut peer_cursor = None;
        let mut file_scheduler = FileDeliveryScheduler::default();
        let mut diagnostic = DeliveryDiagnosticThrottle::default();
        // JoinSet aborts its tasks on drop: retiring the worker also retires
        // every control dial/exchange. Never detach work from this lifetime.
        let mut controls = tokio::task::JoinSet::new();
        let mut control_owners = ControlTaskOwners::default();
        let mut active: std::collections::HashMap<ControlWork, ControlRoutes> =
            std::collections::HashMap::new();
        let mut retries = super::delivery_retry::RetrySchedule::default();
        // Custody qualifies one exact destination, whereas failures pace the
        // peer lane. One held event must not park another queued event.
        let mut quiet_destinations = super::delivery_retry::RetrySchedule::default();
        // Exact-event receipt polls cannot park another message on the peer lane.
        let mut receipt_polls = super::delivery_retry::RetrySchedule::default();
        let mut network = self.network_changes.subscribe();
        let mut roster_changes = self.subscribe_roster_changes();
        let mut roster_open = true;
        loop {
            tokio::select! {
                result = file_scheduler.workers.join_next(), if !file_scheduler.workers.is_empty() => {
                    if let Some(result) = result {
                        if let Err(error) = &result { log::error!("automatic file delivery task failed: {error}"); return; }
                        file_scheduler.finish(&self, result, &mut diagnostic);
                    }
                    continue;
                },
                changed = roster_changes.changed(), if roster_open => {
                    if changed.is_ok() {
                        if !file_scheduler.refresh_held(&self, std::time::Instant::now()) && idle_repair.is_none() {
                            continue;
                        }
                    } else { roster_open = false; continue; }
                },
                _ = interval.tick(), if idle_repair.is_none() => {},
                _ = async {
                    match idle_repair {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {},
                _ = self.delivery_notify.notified() => {},
                _ = network.changed() => {
                    self.clear_delivery_observations();
                    self.sessions.network_changed();
                    file_scheduler.workers.shutdown().await;
                    file_scheduler.running_sockets.clear();
                    file_scheduler.transfers.clear();
                    file_scheduler.relay_cooldowns.clear();
                    file_scheduler.retries.clear();
                    file_scheduler.parked.clear();
                    self.relay_retry_after.lock().expect("relay retry lock not poisoned").clear();
                    controls.shutdown().await;
                    self.clear_delivery_observations();
                    control_owners.clear();
                    active.clear();
                    retries.clear();
                    quiet_destinations.clear();
                    receipt_polls.clear();
                    self.channel_recovery_notify.notify_one();
                    self.account_sync_notify.notify_one();
                    self.bridge_notify.notify_one();
                    if idle_repair.is_none() {
                        continue;
                    }
                },
                result = controls.join_next_with_id(), if !controls.is_empty() => {
                    match result {
                        Some(Ok((task, (key, persistence_failed, progress, observation, fresh_hold)))) => {
                            if control_owners.finished(task).as_ref() == Some(&key)
                                && let Some(routes) = active.remove(&key) {
                                if matches!(key, ControlWork::Pull(_)) {
                                    let reason = match progress {
                                        Reconciliation::Advanced => super::work_observation::reason::PULL_ADVANCED,
                                        Reconciliation::Quiet => super::work_observation::reason::PULL_QUIET,
                                        Reconciliation::Limited => super::work_observation::reason::PULL_DEFERRED,
                                        Reconciliation::More => super::work_observation::reason::PULL_DEFERRED,
                                        Reconciliation::Deferred => super::work_observation::reason::PULL_DEFERRED,
                                    };
                                    self.observe_mailbox_work(reason, None);
                                }
                                let now = std::time::Instant::now();
                                // A fresh authenticated Have can finish after discovery
                                // presence expires or unrelated demand is queued. Capture
                                // those scheduling inputs at completion, while rejecting
                                // any changed endpoint, relay or authorization generation.
                                let current_routes = self.control_routes(&routes.peer);
                                let same_route = current_routes.endpoints == routes.endpoints
                                    && current_routes.relays == routes.relays
                                    && current_routes.policy_generation == routes.policy_generation;
                                let input = if fresh_hold && same_route {
                                    current_routes.retry_routes()
                                } else { routes.retry_routes() };
                                if let ControlWork::Destination(event) = &key {
                                    let hint = (*event, routes.peer.ed25519_pub, routes.peer.x25519_pub);
                                    if fresh_hold && same_route {
                                        receipt_polls.forget(&hint);
                                        quiet_destinations.settled(hint, input.clone(), QUIET_REPAIR_INTERVAL, now);
                                    }
                                    if progress == Reconciliation::Quiet {
                                        let delay = quiet_destinations.remaining(&hint, &input, now)
                                            .unwrap_or(Duration::from_millis(500));
                                        receipt_polls.settled(hint, input.clone(), delay, now);
                                        retries.settled(key.retry_key(&routes.peer), input, Duration::from_millis(500), now);
                                    } else {
                                        progress.schedule(&mut retries, key.retry_key(&routes.peer), input, now);
                                    }
                                } else {
                                    progress.schedule(&mut retries, key.retry_key(&routes.peer), input, now);
                                }
                                if let Some((token, reason)) = observation { self.finish_delivery_observation_with_reason(token, reason); }
                            }
                            if persistence_failed {
                                diagnostic.warn_if_due();
                            }
                        }
                        // A panicked child cannot leave a permanently occupied
                        // lane. Retire this worker and cancel its remaining work.
                        Some(Err(error)) => {
                            if let Some(key) = control_owners.finished(error.id()) { active.remove(&key); }
                            if !error.is_cancelled() {
                                log::error!("automatic delivery control task failed: {error}");
                                return;
                            }
                        }
                        None => {}
                    }
                    continue;
                },
            }
            // Only a successful empty turn may park. Errors fall back to
            // the existing active pace and preserve repair progress.
            idle_repair = None;
            // A new route for an active peer/eligible relay must not wait behind
            // an exchange still blocked on its old endpoint. Refresh timestamps
            // and unrelated peers do not invalidate the bounded control batch.
            self.cancel_changed_delivery_routes(&mut active, &mut control_owners);
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
            #[cfg(test)]
            self.trace_account_client(&self.identity.public(), || {
                "file_delivery_snapshot_started".into()
            });
            let snapshot = work
                .spawn_blocking(move || {
                    #[cfg(test)]
                    if let Some(hook) = node.delivery_snapshot_hook.lock().unwrap().take() {
                        hook();
                    }
                    let mut store = node.delivery.lock().expect("delivery lock not poisoned");
                    node.recover_delivery(&mut store)?;
                    let next = destination_capacity
                        .then(|| store.next_destination(destination_cursor))
                        .flatten();
                    // Scheduling needs only the durable reference. Loading its
                    // ciphertext here would hydrate every parked destination on
                    // each scan before the owner checks its retry deadline.
                    let destination = next;
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
                    Ok::<_, crate::eventlog::LogError>(Some((destination, receipt, files, cursor)))
                })
                .await
                .map(|value| value.into_inner());
            let (destination, receipt, files, cursor) = match snapshot {
                Ok(Ok(Some(snapshot))) => snapshot,
                Ok(Err(error)) => {
                    log::warn!("delivery snapshot deferred: {error}");
                    diagnostic.warn_if_due();
                    continue;
                }
                Err(error) => {
                    log::warn!("delivery snapshot worker failed: {error}");
                    diagnostic.warn_if_due();
                    continue;
                }
                Ok(Ok(None)) => {
                    diagnostic.warn_if_due();
                    continue;
                }
            };
            let has_durable_work = destination.is_some() || receipt.is_some() || !files.is_empty();
            if let Some((id, destination)) = destination {
                destination_cursor = Some(id);
                let key = ControlWork::Destination(destination.event_id);
                let routes = self.control_routes(&destination.device);
                let input = routes.retry_routes();
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
                        &input,
                        std::time::Instant::now(),
                    )
                {
                    let hint = (
                        destination.event_id,
                        destination.device.ed25519_pub,
                        destination.device.x25519_pub,
                    );
                    let held = quiet_destinations
                        .remaining(&hint, &input, std::time::Instant::now())
                        .is_some();
                    if held
                        && destination.receipt_eligible
                        && receipt_polls.ready(&hint, &input, std::time::Instant::now())
                    {
                        let Some(work) = self.runtime_work.admit() else {
                            return;
                        };
                        let node = self.clone();
                        active.insert(key.clone(), routes);
                        let owner = key.clone();
                        let task = controls.spawn(work.track(async move {
                            let progress = node.collect_destination_receipt(id, &destination).await;
                            (key, false, progress, None, false)
                        }));
                        control_owners.insert(owner, task);
                    } else if !held {
                        let Some(work) = self.runtime_work.admit() else {
                            return;
                        };
                        let reading = self.clone();
                        let payload = work
                            .spawn_blocking(move || {
                                let event = reading
                                    .log
                                    .lock()
                                    .expect("log lock not poisoned")
                                    .get(&destination.event_id)?;
                                Ok::<_, crate::eventlog::LogError>(event.map(|event| {
                                    DeliveryDestination {
                                        device: destination.device,
                                        account: destination.account,
                                        event,
                                        receipt_eligible: destination.receipt_eligible,
                                    }
                                }))
                            })
                            .await;
                        let destination = match payload {
                            Ok(payload) => match payload.into_inner() {
                                Ok(Some(destination)) => destination,
                                Ok(None) => continue,
                                Err(error) => {
                                    log::warn!("delivery payload read deferred: {error}");
                                    diagnostic.warn_if_due();
                                    continue;
                                }
                            },
                            Err(error) => {
                                log::warn!("delivery payload worker failed: {error}");
                                diagnostic.warn_if_due();
                                continue;
                            }
                        };
                        let Some(work) = self.runtime_work.admit() else {
                            return;
                        };
                        // This attempt supersedes any earlier quiet observation,
                        // including when it later fails or is cancelled. Returning
                        // to an older route input must not revive the old hint.
                        quiet_destinations.forget(&(
                            destination.event.id,
                            destination.device.ed25519_pub,
                            destination.device.x25519_pub,
                        ));
                        let node = self.clone();
                        // Capture the exact admitted attempt before spawning. A
                        // cancelled task may never be polled; it still owns cleanup,
                        // and cannot publish a new hint after its parent is dropped.
                        let attempt = ObservationAttempt {
                            node: node.clone(),
                            token: node.begin_delivery_observation(id, &destination),
                        };
                        active.insert(key.clone(), routes);
                        let owner = key.clone();
                        let task = controls.spawn(work.track(async move {
                            // Move the entire Drop owner into the future. Capturing
                            // only its Copy token would drop the guard at admission.
                            let mut attempt = attempt;
                            let live = node
                                .delivery
                                .lock()
                                .expect("delivery lock not poisoned")
                                .contains_destination(id, destination.event.id);
                            let (progress, reason, continuation) = if live {
                                node.retry_one_destination_outcome(
                                    &destination,
                                    DeliveryAttemptContext::RetainedRecovery,
                                )
                                .await
                            } else {
                                (false, None, TransferContinuation::None)
                            };
                            let (failed, advanced) = if progress {
                                let mut store =
                                    node.delivery.lock().expect("delivery lock not poisoned");
                                let failed = store.completed_work(id).is_some()
                                    && store.retire_destination(id, destination.event.id).is_err();
                                // An unchanged positive remote-have proof is storage
                                // evidence, not new queue progress. Keep awaiting the
                                // recipient's receipt with the existing bounded backoff.
                                let advanced = !failed
                                    && !store.contains_destination(id, destination.event.id);
                                (failed, advanced)
                            } else {
                                (false, false)
                            };
                            let reconciliation = if failed {
                                Reconciliation::Deferred
                            } else if advanced {
                                Reconciliation::Advanced
                            } else if continuation == TransferContinuation::More {
                                Reconciliation::More
                            } else if continuation == TransferContinuation::Limited {
                                Reconciliation::Limited
                            } else if progress {
                                // Positive custody with an unchanged queue is an
                                // observed success, not a failed delivery attempt.
                                // Keep Awaiting until the recipient's real receipt;
                                // new demand and changed routes retain their wakes.
                                Reconciliation::Quiet
                            } else {
                                Reconciliation::Deferred
                            };
                            let observation = attempt.token.take().map(|token| (token, reason));
                            (key, failed, reconciliation, observation, progress)
                        }));
                        control_owners.insert(owner, task);
                    }
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
                    let owner = key.clone();
                    let task = controls.spawn(work.track(async move {
                        let (progress, _, continuation) = node
                            .retry_one_destination_outcome(
                                &receipt.destination,
                                DeliveryAttemptContext::ReceiptRecovery,
                            )
                            .await;
                        let failed = progress
                            && node
                                .delivery
                                .lock()
                                .expect("delivery lock not poisoned")
                                .finish_receipt(receipt.conversation, receipt.original_event_id)
                                .is_err();
                        (
                            key,
                            failed,
                            Reconciliation::receipt_result(progress, failed, continuation),
                            None,
                            false,
                        )
                    }));
                    control_owners.insert(owner, task);
                }
            }
            file_scheduler.admit(&self, files, cursor);
            file_scheduler.advance(&self);
            // Historical identity authorizes a control pull even with its sender
            // offline. The cursor advances before any bounded network await.
            let explicitly_verified = !self
                .verified_dm_pull_peers
                .lock()
                .expect("verified DM pull mutex not poisoned")
                .is_empty();
            let has_pull_demand = explicitly_verified
                || (self.bridge_directory_allowed()
                    && !self
                        .bridge_dm_pull_peers
                        .lock()
                        .expect("bridge peers mutex not poisoned")
                        .is_empty());
            let peer = if !has_pull_demand
                || active.keys().any(|key| matches!(key, ControlWork::Pull(_)))
            {
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
            // Preserve the circular reconciliation scan and pacing whenever
            // a verified-device or bridge policy can authorize proactive pulls.
            // Check real worker ownership before parking on an empty snapshot.
            if !has_durable_work
                && controls.is_empty()
                && file_scheduler.workers.is_empty()
                && file_scheduler.transfers.is_empty()
                && !has_pull_demand
            {
                // Sparse repair covers a missed local notification or an
                // externally replaced journal. Accepted work and route
                // changes still wake the existing owner immediately.
                idle_repair = Some(tokio::time::Instant::now() + Duration::from_secs(300));
            }
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
                    let owner = key.clone();
                    let task = controls.spawn(work.track(async move {
                        let progress = node.reconcile_delivery_peer(&peer).await;
                        (key, false, progress, None, false)
                    }));
                    control_owners.insert(owner, task);
                }
            }
        }
    }

    pub(in crate::node) async fn reconcile_delivery_peer(
        self: &std::sync::Arc<Self>,
        peer: &crate::discovery::Announce,
    ) -> Reconciliation {
        if !self.verified_dm_pull_peer(&peer.user_id) {
            return Reconciliation::Deferred;
        }
        let conv = super::delivery_receipt::delivery_conversation_id(
            &self.identity.public(),
            &peer.public(),
        );
        let dm = super::conversation::dm_conversation_id(&self.identity.public(), &peer.public());
        tokio::time::timeout(super::route_timing::MAX_CONTROL_DEADLINE, async {
            let (direct, relay) = tokio::join!(
                async {
                    let (messages, receipts) = tokio::join!(
                        Box::pin(self.pull_delivery_scope(peer, dm, false)),
                        Box::pin(self.pull_delivery_scope(peer, conv, false)),
                    );
                    messages.combine(receipts)
                },
                async {
                    // Each relay call drains the same pairwise mailbox; conv
                    // only selects its subsequent local projection. Preserve
                    // serial relay drains instead of duplicating them in flight.
                    let messages = Box::pin(self.pull_delivery_scope(peer, dm, true)).await;
                    let receipts = Box::pin(self.pull_delivery_scope(peer, conv, true)).await;
                    messages.combine(receipts)
                }
            );
            direct.combine(relay)
        })
        .await
        .unwrap_or(Reconciliation::Deferred)
    }

    async fn pull_delivery_scope(
        self: &std::sync::Arc<Self>,
        proof: &crate::discovery::Announce,
        conv: ConversationId,
        relay: bool,
    ) -> Reconciliation {
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
            return Reconciliation::Deferred;
        }
        let peers = if relay {
            let roster = self.roster.lock().expect("roster lock not poisoned");
            self.post_office_candidates_for(&roster, &proof.public())
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
        let mut completed = if relay || !peers.is_empty() {
            Reconciliation::Quiet
        } else {
            Reconciliation::Deferred
        };
        for peer in peers {
            let started = std::time::Instant::now();
            if relay && !self.relay_receive_allowed(&peer.public) {
                continue;
            }
            if !relay && (peer.public != proof.public() || peer.account_id != proof.account_id()) {
                completed = completed.combine(Reconciliation::Deferred);
                continue;
            }
            if relay && !self.relay_retry_due(&peer.public) {
                completed = completed.combine(Reconciliation::Deferred);
                continue;
            }
            let result = self
                .adaptive_control_timeout(&peer.public, async {
                    let Ok(mut channel) = self
                        .privacy_delivery_sync_dial_with_reuse(peer.addr, &peer.public)
                        .await
                        .map(|(channel, _)| channel)
                    else {
                        return Err(SessionError::UnexpectedMessage);
                    };
                    if !current()
                        || (relay && !self.relay_receive_allowed(&peer.public))
                        || channel
                            .peer_announcement()
                            .is_some_and(|p| p.account_id() != peer.account_id)
                    {
                        return Err(SessionError::UnexpectedMessage);
                    }
                    let (advanced, dispatched, complete, limited) = if relay {
                        // Upload confirmations are not incoming progress. A bounded
                        // scalar source observation also sees accepted relay controls.
                        let before = self
                            .log
                            .lock()
                            .expect("log lock")
                            .repository()?
                            .source_revision()?;
                        self.opaque_relay_round(&mut channel, &proof.public(), None)
                            .await?;
                        let after = self
                            .log
                            .lock()
                            .expect("log lock")
                            .repository()?
                            .source_revision()?;
                        (before != after, false, true, false)
                    } else {
                        let store = self.pull_sync_store(channel.peer_identity());
                        let _reception = self
                            .request_reception_scope(&store, &channel)
                            .ok_or(SessionError::UnexpectedMessage)?;
                        let result =
                            super::session::request_sync_turn_paged(&mut channel, &store, conv)
                                .await;
                        let dispatched = self.dispatch_received_sync_work(&store);
                        {
                            let outcome = result?;
                            (
                                outcome.report.applied > 0,
                                dispatched,
                                outcome.complete,
                                outcome.limited,
                            )
                        }
                    };
                    // A quiet success still retires the framed exchange and retries
                    // local projections. It never certifies new data or custody.
                    self.observe_control_exchange(&channel, started);
                    self.recycle_sync_channel(channel);
                    if !dispatched {
                        self.emit_new_messages(conv);
                        self.process_file_events(conv);
                    }
                    Ok::<_, SessionError>((advanced, complete, limited))
                })
                .await;
            if relay {
                self.record_relay_attempt_result(
                    &peer.public,
                    result
                        .as_ref()
                        .map_or(Ok(false), |outcome| outcome.as_ref().map(|_| true)),
                );
            }
            completed = completed.combine(match result {
                Some(Ok((true, _, _))) => Reconciliation::Advanced,
                Some(Ok((false, false, true))) => Reconciliation::Limited,
                Some(Ok((false, false, false))) => Reconciliation::More,
                Some(Ok((false, true, _))) => Reconciliation::Quiet,
                _ => Reconciliation::Deferred,
            });
        }
        completed
    }

    async fn retry_one_destination(
        self: &Arc<Self>,
        destination: &DeliveryDestination,
        context: DeliveryAttemptContext,
    ) -> bool {
        self.retry_one_destination_with_reason(destination, context)
            .await
            .0
    }

    async fn retry_one_destination_with_reason(
        self: &Arc<Self>,
        destination: &DeliveryDestination,
        context: DeliveryAttemptContext,
    ) -> (bool, Option<DeliveryRecoveryReason>) {
        let (held, reason, _) = self
            .retry_one_destination_outcome(destination, context)
            .await;
        (held, reason)
    }

    /// A fresh storage hint changes scheduling only. A real signed receipt is
    /// still required to retire the exact pending destination.
    async fn collect_destination_receipt(
        self: &Arc<Self>,
        logical: EventId,
        destination: &super::delivery_store::DeliveryReference,
    ) -> Reconciliation {
        let current = || {
            !self.delivery_suspended.load(Ordering::Acquire)
                && self.known_account_allowed(&destination.device)
                && self
                    .historical_author(&destination.device.ed25519_pub)
                    .is_some_and(|proof| {
                        proof.public() == destination.device
                            && (destination.account.is_none()
                                || proof.account_id() == destination.account)
                    })
                && self
                    .delivery
                    .lock()
                    .expect("delivery lock not poisoned")
                    .contains_destination(logical, destination.event_id)
        };
        if !current() {
            return Reconciliation::Deferred;
        }
        let Some(peer) = self
            .routing_peer(&destination.device.user_id())
            .filter(|peer| {
                peer.public == destination.device
                    && (destination.account.is_none() || peer.account_id == destination.account)
            })
        else {
            return Reconciliation::Deferred;
        };
        let control = super::delivery_receipt::delivery_conversation_id(
            &self.identity.public(),
            &destination.device,
        );
        let exchange = self.adaptive_control_timeout(&peer.public, async {
            for fresh in [false, true] {
                let started = std::time::Instant::now();
                let admission = super::session_pool::AuthenticationAdmission::RouteProgress;
                let dial = if fresh {
                    self.privacy_dial_with_admission(peer.addr, &peer.public, admission)
                        .await
                        .map(|channel| (channel, false))
                } else {
                    self.privacy_sync_dial_with_admission(peer.addr, &peer.public, admission)
                        .await
                };
                let (mut channel, reused) = dial.map_err(SessionError::from)?;
                if !current()
                    || channel.peer_announcement().is_some_and(|proof| {
                        destination.account.is_some() && proof.account_id() != destination.account
                    })
                {
                    return Err(SessionError::UnexpectedMessage);
                }
                // Receipt publication has its own owner. This collector only
                // pulls the pairwise control scope; it never reopens DM history.
                let store = self.pull_sync_store(channel.peer_identity());
                let _reception = self
                    .request_reception_scope(&store, &channel)
                    .ok_or(SessionError::UnexpectedMessage)?;
                let received = store.lock().expect("sync store lock").reception_notify();
                for tick in 0..4 {
                let result = {
                    let turn =
                        super::session::request_sync_turn_paged(&mut channel, &store, control);
                    tokio::pin!(turn);
                    loop {
                        tokio::select! {
                            result = &mut turn => break result,
                            _ = received.notified() => { self.dispatch_received_sync_work(&store); }
                        }
                    }
                };
                self.dispatch_received_sync_work(&store);
                match result {
                    Ok(outcome) => {
                        self.observe_control_exchange(&channel, started);
                        if current() && !outcome.complete && !outcome.limited && tick + 1 < 4 {
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            continue;
                        }
                        self.recycle_sync_channel(channel);
                        return Ok(if !current() {
                            Reconciliation::Advanced
                        } else if outcome.limited {
                            Reconciliation::Limited
                        } else if !outcome.complete {
                            Reconciliation::More
                        } else {
                            Reconciliation::Quiet
                        });
                    }
                    Err(error) if reused && super::session_pool::retryable_sync_error(&error) => break,
                    Err(error) => {
                        #[cfg(test)]
                        self.trace_account_client(&peer.public, || {
                            format!("receipt_collect_failed error={error}")
                        });
                        return Err(error);
                    }
                }
                }
            }
            Err(SessionError::UnexpectedMessage)
        });
        tokio::pin!(exchange);
        loop {
            let changed = self.delivery_status_notify.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !current() {
                return Reconciliation::Advanced;
            }
            tokio::select! {
                result = &mut exchange => return result.and_then(Result::ok).unwrap_or(Reconciliation::Deferred),
                _ = &mut changed => {},
            }
        }
    }

    async fn retry_one_destination_outcome(
        self: &Arc<Self>,
        destination: &DeliveryDestination,
        context: DeliveryAttemptContext,
    ) -> (bool, Option<DeliveryRecoveryReason>, TransferContinuation) {
        if !self.delivery_destination_allowed(destination) {
            return (false, None, TransferContinuation::None);
        }
        let direct_peer = self
            .routing_peer(&destination.device.user_id())
            .filter(|p| {
                p.public == destination.device
                    && (destination.account.is_none() || p.account_id == destination.account)
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
        // Immediate best-effort flush preserves payload-before-control ordering.
        // Retained recovery can already have a genuine receipt at the recipient,
        // including for a manifest whose bytes still await download consent.
        let independent_receipts =
            destination.receipt_eligible && context == DeliveryAttemptContext::RetainedRecovery;
        let started = std::time::Instant::now();
        let budget = super::route_timing::MAX_CONTROL_DEADLINE;
        let direct_more = std::sync::atomic::AtomicBool::new(false);
        let direct_limited = std::sync::atomic::AtomicBool::new(false);
        let direct = async {
            let mut held = false;
            let mut timed_out = false;
            if let Some(peer) = direct_peer {
                let outcome = self
                    .adaptive_control_timeout(&peer.public, async {
                        let receipt_ready = tokio::sync::Notify::new();
                        if !independent_receipts {
                            receipt_ready.notify_one();
                        }
                        let messages = async {
                            let held = self
                                .transfer_delivery_destination_with_admission(
                                    &peer,
                                    destination,
                                    destination.event.conversation_id,
                                    super::session_pool::AuthenticationAdmission::RouteProgress,
                                    (!independent_receipts).then_some(&receipt_ready),
                                )
                                .await;
                            self.emit_new_messages(destination.event.conversation_id);
                            if held.0 == TransferOutcome::More {
                                direct_more.store(true, Ordering::Release);
                            }
                            if held.0 == TransferOutcome::Limited {
                                direct_limited.store(true, Ordering::Release);
                            }
                            (held.0 == TransferOutcome::Held, held.1)
                        };
                        let receipts = || async {
                            let control = super::delivery_receipt::delivery_conversation_id(
                                &self.identity.public(),
                                &peer.public,
                            );
                            let control_outcome =
                                Box::pin(self.transfer_delivery_destination_with_admission(
                                    &peer,
                                    destination,
                                    control,
                                    super::session_pool::AuthenticationAdmission::RouteProgress,
                                    None,
                                ))
                                .await;
                            if control_outcome.0 == TransferOutcome::More {
                                direct_more.store(true, Ordering::Release);
                            }
                            if control_outcome.0 == TransferOutcome::Limited {
                                direct_limited.store(true, Ordering::Release);
                            }
                            self.emit_new_messages(control);
                        };
                        if independent_receipts {
                            // Existing receipt evidence must not wait behind
                            // ordinary history, and new messages must not wait
                            // behind receipt history. Both scopes remain owned
                            // by this one deadline/cancellation boundary and
                            // the shared TCP capacity. Only receipt validation
                            // may retire this exact destination as delivered.
                            tokio::pin!(messages);
                            let early = receipts();
                            tokio::pin!(early);
                            let held = tokio::select! {
                                result = &mut messages => {
                                    if !result.0 { early.await; }
                                    result.0
                                }
                                _ = &mut early => messages.await.0,
                            };
                            // Positive Have finishes the original-message phase.
                            // The same bounded destination lane collects receipts
                            // separately without reading/resending its ciphertext.
                            if held {
                                direct_more.store(true, Ordering::Release);
                            }
                            held
                        } else {
                            // Immediate flushes, own copies and receipt transfers
                            // keep their payload-before-control exchange contract.
                            let (held, followed) = messages.await;
                            if held && !followed {
                                receipts().await;
                            }
                            held
                        }
                    })
                    .await;
                timed_out = outcome.is_none();
                held = outcome.unwrap_or(false);
            }
            (
                held,
                started.elapsed(),
                timed_out,
                direct_more.load(Ordering::Acquire),
                direct_limited.load(Ordering::Acquire),
            )
        };
        let relay = async {
            let outcome =
                tokio::time::timeout(budget, self.replicate_delivery_destination(destination))
                    .await;
            let timed_out = outcome.is_err();
            (outcome.unwrap_or(false), started.elapsed(), timed_out)
        };
        tokio::pin!(direct, relay);
        // Both alternatives share the same bounded wall-clock window. Original
        // custody cannot cancel recipient progress; exact confirmation can.
        // The receipt worker already owns an immutable signed recipient proof:
        // qualified custody can retire its publication queue without waiting
        // for a stalled alternative. This grants no original delivery status.
        let retired = async {
            loop {
                let changed = self.delivery_status_notify.notified();
                tokio::pin!(changed);
                // Register before checking durable state: notify_waiters does
                // not retain a permit for a future subscriber.
                changed.as_mut().enable();
                if !self
                    .delivery
                    .lock()
                    .expect("delivery lock not poisoned")
                    .contains_work_event(destination.event.id)
                {
                    return;
                }
                changed.await;
            }
        };
        tokio::pin!(retired);
        let mut direct_result = None;
        let mut relay_result = None;
        while direct_result.is_none() || relay_result.is_none() {
            tokio::select! {
                _ = &mut retired => break,
                result = &mut direct, if direct_result.is_none() => {
                    direct_result = Some(result);
                    if context == DeliveryAttemptContext::ReceiptRecovery && result.0 {
                        break;
                    }
                }
                result = &mut relay, if relay_result.is_none() => {
                    relay_result = Some(result);
                    if context == DeliveryAttemptContext::ReceiptRecovery && result.0 {
                        break;
                    }
                }
            }
        }
        // Dropping an unfinished alternative releases its socket and peer lane.
        // Custody or another destination's receipt leaves this event pending.
        let (held, direct_elapsed, direct_timeout, more, limited) =
            direct_result.unwrap_or((false, started.elapsed(), false, false, false));
        let (relay_held, relay_elapsed, relay_timeout) =
            relay_result.unwrap_or((false, started.elapsed(), false));
        if started.elapsed() >= std::time::Duration::from_secs(10) {
            log::warn!(target: "mesh_talk::delivery",
                "slow delivery attempt direct_ms={} relay_ms={} direct_held={} relay_held={} budget_ms={}",
                direct_elapsed.as_millis(), relay_elapsed.as_millis(), held, relay_held, budget.as_millis());
        }
        // This return value is remote storage evidence. The scheduling owner
        // separately checks whether durable destination work actually advanced.
        if context != DeliveryAttemptContext::ReceiptRecovery && (held || relay_held) {
            crate::transport::diagnostics::record_message(|metrics| {
                metrics.direct_storage_holds =
                    metrics.direct_storage_holds.saturating_add(u64::from(held));
                metrics.relay_storage_holds = metrics
                    .relay_storage_holds
                    .saturating_add(u64::from(relay_held));
            });
        }
        let held = held || relay_held;
        (
            held,
            if held || more || limited {
                None
            } else if direct_timeout || relay_timeout {
                Some(DeliveryRecoveryReason::AttemptTimedOut)
            } else {
                reason
            },
            if more {
                TransferContinuation::More
            } else if limited {
                TransferContinuation::Limited
            } else {
                TransferContinuation::None
            },
        )
    }

    fn file_card_manifest_matches(
        &self,
        file: &super::delivery_store::FileCard,
        entry: &super::received_log::ReceivedEntry,
        original: Option<&Event>,
    ) -> bool {
        entry.conversation == file.conversation
            && entry.from == self.user_id()
            && super::files::eligible_manifest_row(entry, original, None).is_some_and(|manifest| {
                manifest.file_conv() == file.file_conversation
                    && manifest.chunk_count() == file.chunk_count
            })
    }

    fn file_card_scope(
        &self,
        file: &super::delivery_store::FileCard,
        destination: &super::delivery_store::FileDestination,
        event: &Event,
    ) -> Option<super::privacy_runtime::ManifestScope> {
        (event.id == destination.binding.event_id
            && event.kind == crate::eventlog::EventKind::FileManifest
            && event.author.ed25519_pub() == &self.identity.public().ed25519_pub
            && event.conversation_id
                == super::conversation::dm_conversation_id(
                    &self.identity.public(),
                    &destination.binding.device,
                )
            && event.conversation_id != file.file_conversation
            && event.verify_signature()
            && event.verify_integrity())
        .then_some(super::privacy_runtime::ManifestScope {
            parent: event.conversation_id,
            author: event.author,
            event: event.id,
        })
    }

    /// The accepted WAL already owns complete signed destinations. Resolve and
    /// validate this overlay before retirement, without a post-retirement read.
    fn prepare_manifest_transaction_scopes(
        &self,
        message: &OutgoingDelivery,
        entry: &super::received_log::ReceivedEntry,
        file: &super::delivery_store::FileCard,
    ) -> Result<Vec<(ConversationId, super::privacy_runtime::ManifestScope)>, LogError> {
        let invalid = || {
            LogError::CorruptFile(
                "accepted manifest journal has incomplete or invalid scope material".into(),
            )
        };
        // A personal file with no transport destinations has no external scopes.
        // The delivery owner validates its local account/profile binding.
        if message.destinations.is_empty() && file.destinations.is_empty() {
            return Ok(Vec::new());
        }
        let events: std::collections::HashMap<_, _> = message
            .destinations
            .iter()
            .map(|destination| (destination.event.id, destination))
            .collect();
        let original = events
            .get(&entry.event_id)
            .map(|destination| &destination.event);
        if message.logical_id != file.id
            || entry.event_id != file.id
            || !self.file_card_manifest_matches(file, entry, original)
        {
            return Err(invalid());
        }
        let mut scopes = Vec::with_capacity(file.destinations.len());
        for destination in &file.destinations {
            let signed = events
                .get(&destination.binding.event_id)
                .ok_or_else(invalid)?;
            if signed.device != destination.binding.device
                || signed.account != destination.binding.account
            {
                return Err(invalid());
            }
            let scope = self
                .file_card_scope(file, destination, &signed.event)
                .ok_or_else(invalid)?;
            scopes.push((file.file_conversation, scope));
        }
        Ok(scopes)
    }

    fn install_file_card_scopes(
        &self,
        scopes: Vec<(ConversationId, super::privacy_runtime::ManifestScope)>,
    ) {
        for (file, scope) in scopes {
            self.remember_manifest_scope(file, scope.parent, scope.author, scope.event);
        }
    }

    async fn retry_file_destination(
        self: &Arc<Self>,
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

    /// Encrypted custody is independent of the recipient's local download
    /// approval. An explicit refusal still stops further custody publication.
    fn file_relay_custody_current(
        &self,
        file: &super::delivery_store::FileCard,
        destination: &super::delivery_store::FileDestination,
    ) -> bool {
        self.file_destination_current(file, destination)
            && match self.outgoing_file_consent_status(file.id, destination.binding.event_id) {
                Ok(status) => status != Some(crate::eventlog::sync::FileConsentStatus::Declined),
                Err(error) => {
                    log::warn!("file custody deferred after consent read failure: {error}");
                    false
                }
            }
    }

    fn file_retry_routes(
        &self,
        file: &super::delivery_store::FileCard,
        destination: &super::delivery_store::FileDestination,
    ) -> FileRoutes {
        #[cfg(test)]
        FILE_RETRY_ROUTE_READS.with(|reads| reads.set(reads.get() + 1));
        let current = self.file_destination_current(file, destination);
        let custody = self.file_relay_custody_current(file, destination);
        let binding = &destination.binding;
        let mut relay_inputs = std::collections::BTreeSet::new();
        let mut endpoints = std::collections::BTreeSet::new();
        if current
            && let Some(peer) = self
                .routing_peer(&binding.device.user_id())
                .filter(|peer| peer.public == binding.device && peer.account_id == binding.account)
        {
            endpoints.insert((
                false,
                peer.public.user_id(),
                peer.account_id.clone(),
                peer.addr,
            ));
            endpoints.extend(
                peer.routes
                    .iter()
                    .filter(|route| {
                        route.source != crate::discovery::roster::RouteSource::PostOffice
                    })
                    .map(|route| {
                        (
                            false,
                            peer.public.user_id(),
                            peer.account_id.clone(),
                            route.addr,
                        )
                    }),
            );
        }
        if custody {
            let relays = {
                let roster = self.roster.lock().expect("roster lock not poisoned");
                self.post_office_candidates_for(&roster, &binding.device)
            };
            relay_inputs.extend(
                relays
                    .into_iter()
                    .filter(|peer| self.relay_send_allowed(&peer.public))
                    .map(|peer| RelayRouteInput::observe(&peer)),
            );
        }
        FileRoutes {
            relays: relay_inputs,
            policy_generation: self.privacy.generation.load(Ordering::Acquire),
            endpoints,
            current,
            custody,
        }
    }

    #[cfg(test)]
    pub(in crate::node) async fn retry_file_step(
        self: &Arc<Self>,
        file: &super::delivery_store::FileCard,
        destination: &super::delivery_store::FileDestination,
        transfers: &mut std::collections::HashMap<(EventId, EventId), FileTransfer>,
    ) -> bool {
        self.retry_file_step_outcome(file, destination, transfers)
            .await
            .complete()
    }

    async fn retry_file_step_outcome(
        self: &Arc<Self>,
        file: &super::delivery_store::FileCard,
        destination: &super::delivery_store::FileDestination,
        transfers: &mut std::collections::HashMap<(EventId, EventId), FileTransfer>,
    ) -> FileStepOutcome {
        self.prune_file_transfers(transfers);
        let key = (file.id, destination.binding.event_id);
        if !self.file_destination_current(file, destination) {
            transfers.remove(&key);
            return FileStepOutcome::Clean(false);
        }
        match self.recipient_completed_file(file, destination) {
            Ok(true) => {
                transfers.remove(&key);
                return FileStepOutcome::Clean(true);
            }
            Ok(false) => {}
            Err(error) => {
                log::warn!("file completion observation deferred: {error}");
                return FileStepOutcome::Failed;
            }
        }
        if !transfers.contains_key(&key) {
            if transfers.len() == MAX_FILE_TRANSFERS {
                // The worker admits only available slots; standalone callers
                // leave excess work durable for the next scheduled turn.
                return FileStepOutcome::Clean(false);
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
            return FileStepOutcome::Clean(false);
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
                FileStepOutcome::Clean(true)
            }
            Ok(Ok(false)) => FileStepOutcome::Clean(false),
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
                FileStepOutcome::Failed
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
        self: &Arc<Self>,
        transfer: &mut FileTransfer,
    ) -> Result<bool, SessionError> {
        let file = &transfer.file;
        let destination = &transfer.destination;
        let binding = &destination.binding;
        if !self.file_destination_current(file, destination) {
            return Err(SessionError::UnexpectedMessage);
        }
        if matches!(transfer.phase, FilePhase::Dial) {
            if transfer.recipient_observation.is_none() {
                let now = std::time::Instant::now();
                let roster = self.roster.lock().expect("roster lock not poisoned");
                let observation = roster
                    .get(&binding.device.user_id())
                    .filter(|peer| {
                        peer.public == binding.device && peer.account_id == binding.account
                    })
                    .map(|peer| super::participant_runtime::FilePullPeer::observe(peer, now));
                transfer.recipient_observation = Some(HeldPeer {
                    public: binding.device.clone(),
                    account: binding.account.clone(),
                    observation,
                });
            }
            let mut candidates = Vec::new();
            if let Some(peer) = self
                .routing_peer(&binding.device.user_id())
                .filter(|peer| peer.public == binding.device && peer.account_id == binding.account)
            {
                candidates.push((false, peer));
            }
            if self.file_relay_custody_current(file, destination) {
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
            transfer.held_observation = relay.then(|| {
                let mut observation = RelayHeldObservation::capture(
                    self,
                    destination,
                    &peer,
                    std::time::Instant::now(),
                );
                observation.peers[0] = transfer
                    .recipient_observation
                    .as_ref()
                    .expect("dial observation captured")
                    .clone();
                observation
            });
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
                let reception = self
                    .request_reception_scope(&store, channel)
                    .ok_or(SessionError::UnexpectedMessage)?;
                let result = transfer.sync.step(channel, &store, conversation).await;
                self.dispatch_received_sync_work(&store);
                drop(reception);
                if result? {
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
                if !transfer.identity_offered {
                    transfer.identity_offered = true;
                    if self
                        .offer_file_message_identity(channel, binding.event_id)
                        .await
                        .is_err()
                    {
                        transfer.channel = None;
                        transfer.phase = FilePhase::Dial;
                        transfer.sync = super::session::SyncProgress::default();
                        return Ok(false);
                    }
                }
                if !transfer.sticker_proof_attempted {
                    transfer.sticker_proof_attempted = true;
                    if let Some(headers) = self.sticker_chunk_proof(file.file_conversation)? {
                        match super::session::request_sticker_file_consent(
                            channel,
                            file.file_conversation,
                            binding.event_id,
                            headers,
                        )
                        .await
                        {
                            Ok(Some(control)) => {
                                let _ = self.accept_file_consent_event(&control, &binding.device);
                            }
                            Ok(None) => {}
                            Err(_) => {
                                // Older endpoints close on optional appended messages.
                                // Reconnect once, then use their ordinary consent path.
                                transfer.channel = None;
                                transfer.phase = FilePhase::Dial;
                                transfer.sync = super::session::SyncProgress::default();
                                return Ok(false);
                            }
                        }
                    }
                }
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
                            transfer.sync = super::session::SyncProgress::default();
                            // A pending local download must not pin this file on
                            // its direct route while encrypted custody is available.
                            if status == crate::eventlog::sync::FileConsentStatus::Pending
                                && transfer.candidate_cursor + 1 < transfer.candidate_count
                            {
                                transfer.candidate_cursor += 1;
                                transfer.phase = FilePhase::Dial;
                            } else {
                                transfer.phase = FilePhase::ConsentWait;
                            }
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
                    .step_burst_paged(
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
            self.file_relay_custody_current(file, destination)
                && self.relay_send_allowed(&relay.public)
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
                let bridge = self.bridge_generation.load(Ordering::SeqCst);
                let outer = self
                    .resolve_opaque_confirmation(opaque, final_chunk, &|| {
                        current()
                            && channel.io_admitted()
                            && (!federated
                                || (self.bridge_directory_allowed()
                                    && self.bridge_generation.load(Ordering::SeqCst) == bridge))
                    })
                    .await?;
                if !current()
                    || !channel.io_admitted()
                    || (federated
                        && (!self.bridge_directory_allowed()
                            || self.bridge_generation.load(Ordering::SeqCst) != bridge))
                {
                    return Err(SessionError::UnexpectedMessage);
                }
                let outer = outer.ok_or(SessionError::NoProgress)?;
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

    async fn request_file_manifest_with_reception(
        self: &Arc<Self>,
        channel: &mut crate::transport::SecureChannel<tokio::net::TcpStream>,
        store: &std::sync::Mutex<super::privacy_sync::GuardedSyncStore<'_>>,
        conversation: ConversationId,
    ) -> Result<(), SessionError> {
        let reception = self
            .request_reception_scope(store, channel)
            .ok_or(SessionError::UnexpectedMessage)?;
        let result = super::session::request_round_paged(channel, store, conversation).await;
        self.dispatch_received_sync_work(store);
        drop(reception);
        result.map(|_| ())
    }

    async fn retry_file_destination_inner(
        self: &Arc<Self>,
        file: &super::delivery_store::FileCard,
        destination: &super::delivery_store::FileDestination,
        direct: bool,
    ) -> bool {
        let binding = &destination.binding;
        let current = || self.file_destination_current(file, destination);
        if !current() {
            return false;
        }
        match self.recipient_completed_file(file, destination) {
            Ok(true) => return true,
            Ok(false) => {}
            Err(error) => {
                log::warn!("file completion observation deferred: {error}");
                return false;
            }
        }
        let manifest_conv =
            super::conversation::dm_conversation_id(&self.identity.public(), &binding.device);
        let mut candidates = Vec::new();
        if direct && let Some(peer) = self.routing_peer(&binding.device.user_id()) {
            candidates.push((false, peer));
        }
        if self.file_relay_custody_current(file, destination) {
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
                    // Reuse the retained worker's guarded publication state
                    // machine so refusal/cancellation/relay-role changes are
                    // rechecked on every opaque payload page in immediate sends.
                    let mut transfer = FileTransfer::new(file.clone(), destination.clone());
                    transfer.route = Some((true, peer.clone()));
                    transfer.channel = Some(channel);
                    transfer.phase = FilePhase::RelayManifestLegacy;
                    #[cfg(test)]
                    phase.store(6, std::sync::atomic::Ordering::Relaxed);
                    while !matches!(transfer.phase, FilePhase::RelayHeld) {
                        if self
                            .advance_relay_file_transfer(&mut transfer)
                            .await
                            .is_err()
                        {
                            return false;
                        }
                    }
                    #[cfg(test)]
                    phase.store(7, std::sync::atomic::Ordering::Relaxed);
                    return self.file_relay_custody_current(file, destination);
                }
                let store = self.sync_store(channel.peer_identity());
                // Publish the manifest first so private recipients/relays can
                // durably reconstruct chunk scope before any chunk disclosure.
                if self
                    .request_file_manifest_with_reception(&mut channel, &store, manifest_conv)
                    .await
                    .is_err()
                {
                    return false;
                }
                if !current() || (relay && !self.relay_send_allowed(&peer.public)) {
                    return false;
                }
                if !relay {
                    if self
                        .offer_file_message_identity(&mut channel, binding.event_id)
                        .await
                        .is_err()
                    {
                        drop(channel);
                        let Ok(reconnected) = self.privacy_dial(peer.addr, &peer.public).await
                        else {
                            return false;
                        };
                        channel = reconnected;
                    }
                    let headers = match self.sticker_chunk_proof(file.file_conversation) {
                        Ok(headers) => headers,
                        Err(error) => {
                            log::warn!("sticker proof lookup deferred: {error}");
                            return false;
                        }
                    };
                    if let Some(headers) = headers {
                        match super::session::request_sticker_file_consent(
                            &mut channel,
                            file.file_conversation,
                            binding.event_id,
                            headers,
                        )
                        .await
                        {
                            Ok(Some(control)) => {
                                let _ = self.accept_file_consent_event(&control, &binding.device);
                            }
                            Ok(None) => {}
                            Err(_) => {
                                drop(channel);
                                let Ok(reconnected) =
                                    self.privacy_dial(peer.addr, &peer.public).await
                                else {
                                    return false;
                                };
                                channel = reconnected;
                            }
                        }
                    }
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
                            drop(channel);
                            let Ok(reconnected) = self.privacy_dial(peer.addr, &peer.public).await
                            else {
                                return false;
                            };
                            channel = reconnected;
                            if self
                                .request_file_manifest_with_reception(
                                    &mut channel,
                                    &store,
                                    manifest_conv,
                                )
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
                            drop(channel);
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
                            if self
                                .request_file_manifest_with_reception(
                                    &mut channel,
                                    &store,
                                    manifest_conv,
                                )
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
                if super::session::request_file_round_paged(
                    &mut channel,
                    &store,
                    file.file_conversation,
                )
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

    pub(in crate::node) async fn flush_file_delivery(self: &Arc<Self>, id: EventId) {
        let file = {
            let mut store = self.delivery.lock().expect("delivery lock not poisoned");
            if let Err(error) = self.recover_delivery(&mut store) {
                log::warn!("delivery recovery deferred: {error}");
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

// Keep local storage failures retryable at the wire delivery admission boundary.
fn delivery_log_error(error: NodeError) -> LogError {
    match error {
        NodeError::Log(error) => error,
        other => LogError::AdmissionRejected(other.to_string()),
    }
}

#[cfg(test)]
mod diagnostic_tests {
    #[tokio::test]
    async fn receipt_recovery_without_direct_or_relay_custody_keeps_durable_queue() {
        use super::super::test_support::node_with_files;
        use crate::discovery::Announce;
        use crate::identity::{account::Account, device::DeviceIdentity};
        let source_dir = tempfile::tempdir().unwrap();
        let receiver_dir = tempfile::tempdir().unwrap();
        let source_identity = DeviceIdentity::generate();
        let receiver_identity = DeviceIdentity::generate();
        let source_account = Account::generate();
        let receiver_account = Account::generate();
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = closed.local_addr().unwrap();
        let source_proof =
            Announce::new_with_account(&source_identity, &source_account, "source", address.port());
        let receiver_proof = Announce::new_with_account(
            &receiver_identity,
            &receiver_account,
            "receiver",
            address.port(),
        );
        let (source, _, _) = node_with_files(
            source_dir.path(),
            source_identity,
            source_account,
            &receiver_proof,
        );
        let (receiver, mut messages, _) = node_with_files(
            receiver_dir.path(),
            receiver_identity,
            receiver_account,
            &source_proof,
        );
        let id = source
            .enqueue_to_account(&receiver.account_id(), b"receipt custody negative", None)
            .await
            .unwrap();
        let original = source
            .delivery
            .lock()
            .unwrap()
            .message(id)
            .unwrap()
            .destinations[0]
            .event
            .clone();
        receiver
            .log
            .lock()
            .unwrap()
            .append_durable(original.clone())
            .unwrap();
        receiver.emit_new_messages(original.conversation_id);
        assert!(messages.try_recv().is_ok());
        let receipt = receiver.delivery.lock().unwrap().retry_receipts(1)[0].clone();
        let relay = Announce::new_post_office(
            &DeviceIdentity::generate(),
            "unavailable relay",
            address.port(),
        );
        receiver
            .roster
            .lock()
            .unwrap()
            .update(&relay, address.ip(), &receiver.user_id());
        drop(closed);
        let held = receiver
            .retry_one_destination(
                &receipt.destination,
                super::DeliveryAttemptContext::ReceiptRecovery,
            )
            .await;
        assert!(
            !held,
            "no completed authenticated holder can qualify receipt retirement"
        );
        assert_eq!(
            receiver.delivery.lock().unwrap().retry_receipts(1)[0]
                .destination
                .event
                .id,
            receipt.destination.event.id
        );
        assert_eq!(
            source.delivery_status(id),
            Some(super::super::DeliveryStatus::Awaiting)
        );
    }

    #[test]
    fn quiet_pull_deadline_and_changed_admission_do_not_requeue_as_progress() {
        use super::Reconciliation::{Advanced, Deferred, Quiet};
        use std::time::{Duration, Instant};
        let mut retries = super::super::delivery_retry::RetrySchedule::default();
        let now = Instant::now();
        Quiet.schedule(&mut retries, "peer", 1, now);
        assert!(!retries.ready(&"peer", &1, now + Duration::from_millis(500)));
        assert!(!retries.ready(&"peer", &1, now + Duration::from_secs(59)));
        assert!(
            !retries.ready(&"peer", &1, now + Duration::from_secs(299)),
            "unchanged successful reconciliation needs only sparse five-minute repair"
        );
        assert!(retries.ready(&"peer", &1, now + super::QUIET_REPAIR_INTERVAL));
        assert!(
            retries.ready(&"peer", &2, now),
            "new route or verified policy wakes immediately"
        );
        // Capture the admitted stamp: a completion must not label old work with
        // a newer route and suppress its replacement.
        Quiet.schedule(&mut retries, "peer", 1, now);
        assert!(retries.ready(&"peer", &2, now));
        Deferred.schedule(&mut retries, "failed", 1, now);
        assert!(!retries.ready(&"failed", &1, now + Duration::from_millis(500)));
        assert!(retries.ready(&"failed", &1, now + Duration::from_secs(2)));
        Advanced.schedule(&mut retries, "peer", 1, now);
        assert!(retries.ready(&"peer", &1, now + Duration::from_millis(500)));
        assert_eq!(Quiet.combine(Deferred), Deferred);
        assert_eq!(Advanced.combine(Deferred), Advanced);
    }

    #[test]
    fn refreshing_same_verified_pull_set_does_not_invalidate_quiet_deadline() {
        use super::super::test_support::node_with_files;
        use crate::discovery::Announce;
        use crate::identity::{account::Account, device::DeviceIdentity};
        let directory = tempfile::tempdir().unwrap();
        let remote = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&remote, &Account::generate(), "peer", 9);
        let (node, _, _) = node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let peer = proof.public();
        node.replace_verified_dm_pull_peers(vec![proof.user_id.clone()]);
        let admitted = node.control_routes(&peer).retry_routes();
        let mut retries = super::super::delivery_retry::RetrySchedule::default();
        let now = std::time::Instant::now();
        super::Reconciliation::Quiet.schedule(&mut retries, "peer", admitted.clone(), now);
        for _ in 0..16 {
            node.replace_verified_dm_pull_peers(vec![proof.user_id.to_ascii_uppercase()]);
        }
        let refreshed = node.control_routes(&peer).retry_routes();
        assert_eq!(admitted, refreshed);
        assert!(!retries.ready(&"peer", &refreshed, now));
        node.replace_verified_dm_pull_peers(Vec::new());
        node.replace_verified_dm_pull_peers(vec![proof.user_id.clone()]);
        assert!(
            retries.ready(&"peer", &node.control_routes(&peer).retry_routes(), now),
            "reverification must wake retained pull work"
        );
    }

    #[tokio::test]
    async fn control_known_route_preference_renewals_preserve_owner_and_retry_deadline() {
        use super::super::test_support::node_with_files;
        use crate::discovery::Announce;
        use crate::identity::{account::Account, device::DeviceIdentity};
        let dir = tempfile::tempdir().unwrap();
        let peer = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "Peer",
            9,
        );
        let (node, _, _) = node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &peer,
        );
        let relay_identity = DeviceIdentity::generate();
        let relay = Announce::new_post_office(&relay_identity, "Relay", 12345);
        for ip in ["127.0.0.1", "127.0.0.2"] {
            node.roster
                .lock()
                .unwrap()
                .update(&relay, ip.parse().unwrap(), &node.user_id());
        }
        let initial = node.control_routes(&peer.public());
        let input = initial.retry_routes();
        let mut retries = super::super::delivery_retry::RetrySchedule::default();
        let now = std::time::Instant::now();
        retries.settled(
            "peer",
            input.clone(),
            std::time::Duration::from_secs(60),
            now,
        );
        let key = super::ControlWork::Pull(peer.user_id.clone());
        let mut tasks = tokio::task::JoinSet::new();
        let task = tasks.spawn(std::future::pending::<()>());
        let id = task.id();
        let mut owners = super::ControlTaskOwners::default();
        owners.insert(key.clone(), task);
        let mut active = std::collections::HashMap::from([(key.clone(), initial)]);
        let baseline = node
            .roster
            .lock()
            .unwrap()
            .get(&relay.user_id)
            .unwrap()
            .scheduling_endpoints();
        let mut preferred = node
            .roster
            .lock()
            .unwrap()
            .get(&relay.user_id)
            .unwrap()
            .addr;
        for index in 0..8 {
            let ip = if index % 2 == 0 {
                "127.0.0.1"
            } else {
                "127.0.0.2"
            };
            {
                let mut roster = node.roster.lock().unwrap();
                roster.update(&relay, ip.parse().unwrap(), &node.user_id());
                let record = roster.get(&relay.user_id).unwrap();
                assert_eq!(record.scheduling_endpoints(), baseline);
                assert_ne!(record.addr, preferred);
                preferred = record.addr;
            }
            node.cancel_changed_delivery_routes(&mut active, &mut owners);
            assert!(
                active.contains_key(&key),
                "known route preference must not cancel current control owner"
            );
            assert_eq!(owners.current[&key].id(), id);
            assert!(!owners.current[&key].is_finished());
            let refreshed = node.control_routes(&peer.public()).retry_routes();
            assert_eq!(refreshed, input);
            assert!(
                !retries.ready(&"peer", &refreshed, now),
                "known route preference must preserve future quiet deadline"
            );
        }
        node.roster
            .lock()
            .unwrap()
            .update(&relay, "127.0.0.3".parse().unwrap(), &node.user_id());
        assert!(retries.ready(
            &"peer",
            &node.control_routes(&peer.public()).retry_routes(),
            now
        ));
        node.cancel_changed_delivery_routes(&mut active, &mut owners);
        assert!(
            !active.contains_key(&key),
            "new eligible endpoint must retire old control owner"
        );
        assert!(!owners.current.contains_key(&key));
        let before_policy = node.control_routes(&peer.public()).retry_routes();
        node.privacy
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert_ne!(
            node.control_routes(&peer.public()).retry_routes(),
            before_policy
        );
        let before_role = node.control_routes(&peer.public()).retry_routes();
        let changed = Announce::new(&relay_identity, "Relay", relay.tcp_port);
        node.roster
            .lock()
            .unwrap()
            .update(&changed, "127.0.0.1".parse().unwrap(), &node.user_id());
        assert_ne!(
            node.control_routes(&peer.public()).retry_routes(),
            before_role
        );
        tasks.shutdown().await;
    }

    #[tokio::test]
    async fn file_known_route_preference_renewals_preserve_parked_continuation_and_retry_deadline()
    {
        use super::super::test_support::node_with_files;
        use crate::discovery::Announce;
        use crate::identity::{account::Account, device::DeviceIdentity};
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let peer = Announce::new_with_account(&DeviceIdentity::generate(), &account, "Peer", 9);
        let (node, _, _) = node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &peer,
        );
        let relay_identity = DeviceIdentity::generate();
        let relay = Announce::new_post_office(&relay_identity, "Relay", 12345);
        for ip in ["127.0.0.1", "127.0.0.2"] {
            node.roster
                .lock()
                .unwrap()
                .update(&relay, ip.parse().unwrap(), &node.user_id());
        }
        let path = dir.path().join("retained-candidate.bin");
        std::fs::write(&path, b"retained candidate traversal").unwrap();
        let (logical, _) = node
            .enqueue_file_to_account(&account.account_id(), &path, crate::file::FileKind::File)
            .await
            .unwrap();
        let card = node
            .delivery
            .lock()
            .unwrap()
            .file_card(logical)
            .unwrap()
            .clone();
        let destination = card.destinations[0].clone();
        let key = (logical, destination.binding.event_id);
        let routes = node.file_retry_routes(&card, &destination);
        assert!(routes.current && routes.custody);
        let now = std::time::Instant::now();
        let mut scheduler = super::FileDeliveryScheduler::default();
        let mut diagnostic = super::DeliveryDiagnosticThrottle::default();
        let token = scheduler
            .retries
            .reserve(key, routes.clone(), now)
            .unwrap()
            .0;
        let mut transfer = super::FileTransfer::new(card.clone(), destination.clone());
        transfer.candidate_cursor = 1;
        transfer.candidate_count = 2;
        transfer.identity_offered = true;
        scheduler.finish_at(
            &node,
            Ok((
                key,
                std::collections::HashMap::from([(key, transfer)]),
                super::FileStepCompletion {
                    token,
                    routes: routes.clone(),
                    outcome: super::FileStepOutcome::Failed,
                },
            )),
            &mut diagnostic,
            now,
        );
        let baseline = node
            .roster
            .lock()
            .unwrap()
            .get(&relay.user_id)
            .unwrap()
            .scheduling_endpoints();
        let mut preferred = node
            .roster
            .lock()
            .unwrap()
            .get(&relay.user_id)
            .unwrap()
            .addr;
        for index in 0..8 {
            let ip = if index % 2 == 0 {
                "127.0.0.1"
            } else {
                "127.0.0.2"
            };
            {
                let mut roster = node.roster.lock().unwrap();
                roster.update(&relay, ip.parse().unwrap(), &node.user_id());
                let record = roster.get(&relay.user_id).unwrap();
                assert_eq!(record.scheduling_endpoints(), baseline);
                assert_ne!(record.addr, preferred);
                preferred = record.addr;
            }
            scheduler.admit_at(&node, vec![(card.clone(), destination.clone())], None, now);
            assert!(
                scheduler.transfers.is_empty(),
                "known relay preference must not bypass file failure deadline"
            );
            assert_eq!(scheduler.parked[&key].candidate_cursor, 1);
            assert!(scheduler.parked[&key].identity_offered);
            assert_eq!(
                scheduler.parked[&key].routes,
                node.file_retry_routes(&card, &destination)
            );
        }
        scheduler.admit_at(
            &node,
            vec![(card.clone(), destination.clone())],
            None,
            now + std::time::Duration::from_secs(2),
        );
        assert_eq!(scheduler.transfers[&key].candidate_cursor, 1);
        assert!(scheduler.transfers[&key].identity_offered);
        node.roster
            .lock()
            .unwrap()
            .update(&relay, "127.0.0.3".parse().unwrap(), &node.user_id());
        assert_ne!(
            node.file_retry_routes(&card, &destination),
            routes,
            "new relay endpoint must invalidate old continuation input"
        );
        let before_policy = node.file_retry_routes(&card, &destination);
        node.privacy
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert_ne!(node.file_retry_routes(&card, &destination), before_policy);
        let before_role = node.file_retry_routes(&card, &destination);
        let changed = Announce::new(&relay_identity, "Relay", relay.tcp_port);
        node.roster
            .lock()
            .unwrap()
            .update(&changed, "127.0.0.1".parse().unwrap(), &node.user_id());
        assert_ne!(node.file_retry_routes(&card, &destination), before_role);
    }

    #[tokio::test]
    async fn cancelled_old_control_owner_cannot_remove_its_same_key_replacement() {
        let key = super::ControlWork::Pull("fixture".into());
        let mut tasks = tokio::task::JoinSet::new();
        let mut owners = super::ControlTaskOwners::default();
        let old = tasks.spawn(std::future::pending::<()>());
        let old_id = old.id();
        owners.insert(key.clone(), old);
        owners.cancel(&key);
        let replacement = tasks.spawn(std::future::pending::<()>());
        let replacement_id = replacement.id();
        owners.insert(key.clone(), replacement);
        let error = tasks.join_next_with_id().await.unwrap().unwrap_err();
        assert!(error.is_cancelled());
        assert_eq!(error.id(), old_id);
        assert!(owners.finished(error.id()).is_none());
        assert_eq!(owners.current[&key].id(), replacement_id);
        tasks.shutdown().await;
    }

    #[tokio::test]
    async fn unrelated_pull_route_changes_preserve_slow_noise_delivery_and_its_observation() {
        use super::super::test_support::node_with_files;
        use crate::discovery::Announce;
        use crate::identity::{account::Account, device::DeviceIdentity};
        use std::sync::Arc;
        use std::time::Duration;
        let source_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
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
        let (sender, _, _) = node_with_files(source_dir.path(), alice, aa, &bp);
        let (recipient, _, _) = node_with_files(target_dir.path(), bob, bob_account, &ap);
        let unrelated_identity = DeviceIdentity::generate();
        let unrelated_account = Account::generate();
        let unrelated =
            Announce::new_with_account(&unrelated_identity, &unrelated_account, "Unrelated", 9);
        sender.roster.lock().unwrap().update(
            &unrelated,
            "127.0.0.1".parse().unwrap(),
            &sender.user_id(),
        );
        let logical = sender
            .enqueue_to_account(&recipient.account_id(), b"synthetic slow delivery", None)
            .await
            .unwrap();
        let destination = sender
            .delivery
            .lock()
            .unwrap()
            .message(logical)
            .unwrap()
            .destinations[0]
            .clone();
        let healthy = super::ControlWork::Destination(destination.event.id);
        let changing = super::ControlWork::Pull(unrelated.user_id.clone());
        let (accepted, accepted_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        let server_node = recipient.clone();
        let server = tokio::spawn(async move {
            let mut held = Some((accepted, release_rx));
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    incoming = listener.accept() => {
                        let (stream, _) = incoming.unwrap();
                        let node = server_node.clone();
                        let boundary = held.take();
                        connections.spawn(async move {
                            let channel = node.privacy_accept(stream).await.unwrap();
                            if let Some((accepted, release)) = boundary {
                                let _ = accepted.send(());
                                release.await.unwrap();
                            }
                            node.serve_connection(channel).await;
                        });
                    }
                    result = connections.join_next(), if !connections.is_empty() => { result.unwrap().unwrap(); }
                }
            }
        });
        let mut active = std::collections::HashMap::new();
        let mut owners = super::ControlTaskOwners::default();
        let mut controls = tokio::task::JoinSet::new();
        active.insert(healthy.clone(), sender.control_routes(&bp.public()));
        let attempt = super::ObservationAttempt {
            node: sender.clone(),
            token: sender.begin_delivery_observation(logical, &destination),
        };
        assert!(attempt.token.is_some());
        let sending = Arc::clone(&sender);
        let task = controls.spawn(async move {
            let _attempt = attempt;
            let held = sending
                .retry_one_destination_with_reason(
                    &destination,
                    super::DeliveryAttemptContext::RetainedRecovery,
                )
                .await
                .0;
            if held {
                let reference = super::super::delivery_store::DeliveryReference {
                    device: destination.device.clone(),
                    account: destination.account.clone(),
                    event_id: destination.event.id,
                    receipt_eligible: destination.receipt_eligible,
                };
                // Preserve this exact task/observation while driving the new
                // receipt phase. No repeat original scope or source rescue.
                while sending.delivery_status(logical) != Some(super::DeliveryStatus::Delivered) {
                    let progress = sending
                        .collect_destination_receipt(logical, &reference)
                        .await;
                    if progress != super::Reconciliation::More {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
            held
        });
        let healthy_task = task.id();
        owners.insert(healthy.clone(), task);
        tokio::time::timeout(Duration::from_secs(3), accepted_rx)
            .await
            .unwrap()
            .unwrap();
        for port in 10..13 {
            active.insert(changing.clone(), sender.control_routes(&unrelated.public()));
            let pull = controls.spawn(std::future::pending::<bool>());
            owners.insert(changing.clone(), pull);
            let relocated = Announce::new_with_account(
                &unrelated_identity,
                &unrelated_account,
                "Unrelated",
                port,
            );
            sender.roster.lock().unwrap().update(
                &relocated,
                "127.0.0.1".parse().unwrap(),
                &sender.user_id(),
            );
            sender.cancel_changed_delivery_routes(&mut active, &mut owners);
            assert!(
                active.contains_key(&healthy),
                "an unrelated Pull route must not cancel the healthy destination"
            );
            assert_eq!(owners.current[&healthy].id(), healthy_task);
            assert!(!owners.current[&healthy].is_finished());
            assert!(
                sender.account_delivery_observations(&recipient.account_id(), &[logical])[0]
                    .2
                    .is_some(),
                "route churn for another owner cannot erase this exact destination's observation"
            );
        }
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match controls.join_next_with_id().await.unwrap() {
                    Ok((task, progress)) => {
                        assert_eq!(owners.finished(task), Some(healthy.clone()));
                        assert!(progress);
                        break;
                    }
                    Err(error) => {
                        assert!(error.is_cancelled());
                        assert!(owners.finished(error.id()).is_none());
                    }
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            sender.delivery_status(logical),
            Some(super::DeliveryStatus::Delivered),
            "the original signed recipient receipt, rather than relay custody, must finish the delivery"
        );
        server.abort();
        let _ = server.await;
    }
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
        assert_eq!(
            second[0].public, first[0].public,
            "keep the best candidate in every batch"
        );
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
        let candidates = node.post_office_candidates(&node.roster.lock().unwrap());
        let fastest = candidates.last().unwrap();
        for relay in &candidates {
            node.sessions.control_succeeded(
                &relay.public,
                relay.addr,
                std::time::Duration::from_millis(if relay.public == fastest.public {
                    10
                } else {
                    900
                }),
            );
        }
        assert_eq!(
            node.post_office_candidates(&node.roster.lock().unwrap())[0].public,
            fastest.public
        );
        for error in [
            crate::node::session::SessionError::ResourceUnavailable,
            crate::node::session::SessionError::Store(crate::eventlog::LogError::StaleCursor),
        ] {
            assert!(!node.record_relay_attempt_result(&fastest.public, Err(&error)));
            assert!(node.relay_retry_due(&fastest.public));
            assert_eq!(
                node.post_office_candidates(&node.roster.lock().unwrap())[0].public,
                fastest.public
            );
        }
        let failure = crate::node::session::SessionError::Transport(
            crate::transport::TransportError::Io(std::io::ErrorKind::ConnectionRefused.into()),
        );
        assert!(!node.record_relay_attempt_result(&fastest.public, Err(&failure)));
        assert_ne!(
            node.post_office_candidates(&node.roster.lock().unwrap())[0].public,
            fastest.public
        );
        node.record_relay_attempt(&fastest.public, true);
        assert_eq!(
            node.post_office_candidates(&node.roster.lock().unwrap())[0].public,
            fastest.public
        );
    }

    #[tokio::test]
    async fn targeted_relay_rank_balances_origin_distance_and_responsiveness() {
        use crate::discovery::Announce;
        use crate::identity::{account::Account, device::DeviceIdentity};
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let peer = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "Remote sender",
            5000,
        );
        let (node, _, _) = super::super::test_support::node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &peer,
        );
        let near = DeviceIdentity::generate();
        let far = DeviceIdentity::generate();
        let near_addr = "127.0.0.1:5001".parse().unwrap();
        let far_addr = "127.0.0.1:5002".parse().unwrap();
        let mut roster = crate::discovery::roster::Roster::default();
        for (relay, addr, hops) in [(&near, near_addr, 0), (&far, far_addr, 2)] {
            let addr: std::net::SocketAddr = addr;
            roster.update(
                &Announce::new_post_office(relay, "Relay", addr.port()),
                addr.ip(),
                &node.user_id(),
            );
            roster.update_relayed(&peer, addr, &node.user_id());
            roster.set_directory_expiry(
                &peer.user_id,
                addr,
                std::time::Instant::now() + Duration::from_secs(15),
                1,
            );
            roster.set_directory_origin_hops(&peer.user_id, addr, 1, Some(hops));
        }

        let first = || {
            node.post_office_candidates_for(&roster, &peer.public())[0]
                .public
                .clone()
        };
        assert_eq!(first(), near.public());

        node.sessions
            .control_succeeded(&near.public(), near_addr, Duration::from_millis(400));
        node.sessions
            .control_succeeded(&far.public(), far_addr, Duration::from_millis(20));
        assert_eq!(first(), far.public());

        node.sessions.control_timed_out(&far.public(), far_addr);
        assert_eq!(first(), near.public());
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
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let server_node = b.clone();
        let mut server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("server TCP accept");
            accepted_tx.send(()).expect("accepted TCP observer");
            release_rx.await.expect("handshake release");
            let channel = server_node
                .privacy_accept(stream)
                .await
                .expect("server authenticated handshake");
            server_node.serve_connection(channel).await;
        });
        let card = a.delivery.lock().unwrap().file_card(id).unwrap().clone();
        let destination = card.destinations[0].clone();
        let mut scheduler = super::FileDeliveryScheduler::default();
        scheduler.admit(&a, vec![(card, destination)], None);
        let mut diagnostics = super::DeliveryDiagnosticThrottle::default();
        let dial_deadline = tokio::time::Instant::now() + super::FILE_EXCHANGE_TIMEOUT;
        scheduler.advance(&a);
        tokio::select! {
            result = scheduler.workers.join_next() => panic!(
                "Dial worker completed before TCP acceptance: {:?}",
                result.map(|result| result.map(|(_, _, complete)| complete))
            ),
            result = &mut server => panic!("server exited before TCP acceptance: {result:?}"),
            accepted = tokio::time::timeout_at(dial_deadline, accepted_rx) => {
                accepted.expect("Dial TCP acceptance exceeded file exchange deadline")
                    .expect("server dropped TCP acceptance signal");
            }
        }
        // Prove the scheduler retains a valid slow handshake independently of
        // the later durable manifest, consent and chunk exchanges.
        tokio::select! {
            biased;
            result = scheduler.workers.join_next() => panic!(
                "Dial worker completed during held 600ms handshake: {:?}",
                result.map(|result| result.map(|(_, _, complete)| complete))
            ),
            result = &mut server => panic!("server exited during held handshake: {result:?}"),
            () = tokio::time::sleep(std::time::Duration::from_millis(600)) => {}
        }
        release_tx
            .send(())
            .expect("server awaiting handshake release");
        let result = tokio::select! {
            result = &mut server => panic!("server exited during Dial: {result:?}"),
            result = tokio::time::timeout_at(dial_deadline, scheduler.workers.join_next()) => {
                result.expect("Dial exceeded original file exchange deadline")
                    .expect("Dial worker missing").expect("Dial worker panicked")
            }
        };
        scheduler.finish(&a, Ok(result), &mut diagnostics);
        // Each phase gets one existing exchange budget, shared by all of its
        // turns. Fixed phase order rejects redials and bounds total work.
        for phase in [
            super::FilePhase::Manifest,
            super::FilePhase::ManifestCustody,
            super::FilePhase::Consent,
            super::FilePhase::InitialCustody,
            super::FilePhase::Chunks,
            super::FilePhase::FinalCustody,
        ] {
            let current = scheduler
                .transfers
                .values()
                .next()
                .unwrap_or_else(|| panic!("transfer missing before {phase:?}"));
            assert_eq!(
                std::mem::discriminant(&current.phase),
                std::mem::discriminant(&phase),
                "unexpected phase before {phase:?}: {:?}",
                current.phase
            );
            let deadline = tokio::time::Instant::now() + super::FILE_EXCHANGE_TIMEOUT;
            tokio::time::timeout_at(deadline, async {
                loop {
                    scheduler.advance(&a);
                    let result = tokio::select! {
                        // Final custody can release the socket in the same poll
                        // that completes this worker, allowing a normal server EOF.
                        biased;
                        result = scheduler.workers.join_next() => result
                            .unwrap_or_else(|| panic!("worker missing during {phase:?}"))
                            .unwrap_or_else(|error| panic!("worker panicked during {phase:?}: {error}")),
                        result = &mut server => panic!("server exited during {phase:?}: {result:?}"),
                    };
                    scheduler.finish(&a, Ok(result), &mut diagnostics);
                    if scheduler.transfers.values().next().is_none_or(|transfer| {
                        std::mem::discriminant(&transfer.phase) != std::mem::discriminant(&phase)
                    }) {
                        break;
                    }
                }
            }).await.unwrap_or_else(|_| panic!("{phase:?} exceeded file exchange deadline"));
        }
        server.abort();
        if let Err(error) = server.await {
            assert!(error.is_cancelled(), "server panicked: {error}");
        }
        assert!(
            a.outgoing_file_progress(id)
                .is_some_and(|progress| progress.delivered),
            "completed ordered exchanges must confirm delivery"
        );
        assert_eq!(b.read_file(file).unwrap(), b"valid slow route");
    }

    #[tokio::test]
    async fn failed_file_routes_release_capacity_and_do_not_retry_on_unrelated_wakes() {
        use super::super::test_support::node_with_files;
        use crate::discovery::Announce;
        use crate::identity::{account::Account, device::DeviceIdentity};
        let source = tempfile::tempdir().unwrap();
        let bob = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = Announce::new_with_account(&bob, &account, "Bob", 9);
        let (node, _, _) = node_with_files(
            source.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        for index in 0..9 {
            let path = source.path().join(format!("missing-route-{index}.bin"));
            std::fs::write(&path, format!("distinct file {index}")).unwrap();
            node.enqueue_file_to_account(&account.account_id(), &path, crate::file::FileKind::File)
                .await
                .unwrap();
        }
        node.roster.lock().unwrap().forget(&proof.public());
        let mut scheduler = super::FileDeliveryScheduler::default();
        let mut diagnostics = super::DeliveryDiagnosticThrottle::default();
        let now = std::time::Instant::now();
        let mut attempted = std::collections::HashSet::new();
        for _ in 0..4 {
            let scan = scheduler.scan(&node);
            let (files, cursor) = scan.read(&node.delivery.lock().unwrap());
            scheduler.admit_at(&node, files, cursor, now);
            for key in scheduler.transfers.keys() {
                assert!(
                    attempted.insert(*key),
                    "unrelated wake retried a failed route before its deadline"
                );
            }
            // Eight transfers share fewer socket slots. Drain both bounded
            // batches before judging failed work, including first admissions.
            for _ in 0..2 {
                scheduler.advance(&node);
                while let Some(result) = scheduler.workers.join_next().await {
                    scheduler.finish_at(&node, result, &mut diagnostics, now);
                }
            }
            assert!(
                scheduler.transfers.is_empty(),
                "failed routes must release all transfer slots"
            );
        }
        assert_eq!(
            attempted.len(),
            9,
            "cooling destinations must not starve the ninth durable destination"
        );
        assert_eq!(node.delivery.lock().unwrap().file_cards().count(), 9);
    }

    #[tokio::test]
    async fn file_scheduler_cooldown_skips_route_work_until_scan_releases_hold() {
        use super::super::test_support::node_with_files;
        use crate::discovery::Announce;
        use crate::identity::{account::Account, device::DeviceIdentity};
        let source = tempfile::tempdir().unwrap();
        let bob = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = Announce::new_with_account(&bob, &account, "Bob", 9);
        let (node, _, _) = node_with_files(
            source.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let path = source.path().join("cooldown.bin");
        std::fs::write(&path, b"durable held destination").unwrap();
        let (id, _) = node
            .enqueue_file_to_account(&account.account_id(), &path, crate::file::FileKind::File)
            .await
            .unwrap();
        let card = node.delivery.lock().unwrap().file_card(id).unwrap().clone();
        let destination = card.destinations[0].clone();
        let key = (id, destination.binding.event_id);
        let mut scheduler = super::FileDeliveryScheduler::default();
        let now = std::time::Instant::now();
        let until = now + super::FILE_RELAY_RETRY;
        scheduler.relay_cooldowns.push_back(super::FileHold {
            key,
            until,
            kind: super::FileHoldKind::RelayHeld(Box::new(super::RelayHeldObservation::capture(
                &node,
                &destination,
                &node.routing_peer(&bob.public().user_id()).unwrap(),
                now,
            ))),
        });
        super::FILE_RETRY_ROUTE_READS.with(|reads| reads.set(0));
        for _ in 0..4 {
            scheduler.admit_at(
                &node,
                vec![(card.clone(), destination.clone())],
                Some(key),
                now,
            );
        }
        assert!(scheduler.transfers.is_empty());
        assert_eq!(scheduler.relay_cooldowns.front().unwrap().until, until);
        assert_eq!(
            super::FILE_RETRY_ROUTE_READS.with(std::cell::Cell::get),
            0,
            "held selections must not run real routing/consent preparation"
        );
        // Admission never bypasses a retained hold, even with a later clock.
        scheduler.admit_at(
            &node,
            vec![(card.clone(), destination.clone())],
            None,
            until,
        );
        assert!(scheduler.transfers.is_empty());
        assert_eq!(super::FILE_RETRY_ROUTE_READS.with(std::cell::Cell::get), 0);
        scheduler.relay_cooldowns.front_mut().unwrap().until = now;
        scheduler.scan(&node);
        assert!(scheduler.relay_cooldowns.is_empty());
        scheduler.admit(&node, vec![(card.clone(), destination.clone())], None);
        assert_eq!(super::FILE_RETRY_ROUTE_READS.with(std::cell::Cell::get), 1);
        assert!(scheduler.transfers.remove(&key).is_some());

        // Independent roster watch: same-endpoint heartbeats refresh the
        // baseline without rearming a held file; unrelated peers do not rearm it.
        let mut roster_changes = node.subscribe_roster_changes();
        let peer = node.routing_peer(&bob.public().user_id()).unwrap();
        let observation = super::RelayHeldObservation::capture(&node, &destination, &peer, now);
        scheduler.relay_cooldowns.push_back(super::FileHold {
            key,
            until,
            kind: super::FileHoldKind::RelayHeld(Box::new(observation)),
        });
        node.roster.lock().unwrap().update(
            &proof,
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            &node.user_id(),
        );
        roster_changes.changed().await.unwrap();
        scheduler.refresh_held(&node, now);
        assert_eq!(scheduler.relay_cooldowns.len(), 1);
        let stranger = Announce::new(&DeviceIdentity::generate(), "Unrelated", 17);
        node.roster.lock().unwrap().update(
            &stranger,
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            &node.user_id(),
        );
        roster_changes.changed().await.unwrap();
        scheduler.refresh_held(&node, now);
        assert_eq!(scheduler.relay_cooldowns.len(), 1);
        // Exact same binding and endpoint, but the previously observed public
        // lease was stale: current fresh evidence must promptly release it.
        let mut stale = peer.clone();
        for route in &mut stale.routes {
            route.last_seen = now - std::time::Duration::from_secs(3600);
            route.presence_expires_at = None;
        }
        if let super::FileHoldKind::RelayHeld(observation) = &mut scheduler.relay_cooldowns[0].kind
        {
            observation.peers[0].observation = Some(
                super::super::participant_runtime::FilePullPeer::observe(&stale, now),
            );
        }
        scheduler.refresh_held(&node, now);
        assert!(scheduler.relay_cooldowns.is_empty());

        // An actual signed, durable Accepted decision releases ConsentWait in scan.
        scheduler.relay_cooldowns.push_back(super::FileHold {
            key,
            until,
            kind: super::FileHoldKind::ConsentWait,
        });
        let accepted = super::super::file_consent::Decision {
            file: card.file_conversation,
            manifest: destination.binding.event_id,
            sender: node.identity.public(),
            recipient: bob.public(),
            sender_account: Some(node.account_id()),
            recipient_account: account.account_id(),
            decision: super::super::ReceiveDecision::Accepted,
            completion: None,
        }
        .event(&bob, 1)
        .unwrap();
        node.accept_file_consent_event(&accepted, &bob.public())
            .unwrap();
        assert_eq!(
            node.outgoing_file_consent_status(key.0, key.1).unwrap(),
            Some(crate::eventlog::sync::FileConsentStatus::Accepted)
        );
        scheduler.scan(&node);
        assert!(scheduler.relay_cooldowns.is_empty());
        scheduler.admit(&node, vec![(card.clone(), destination.clone())], None);
        assert!(scheduler.transfers.remove(&key).is_some());

        // A held hint cannot authorize stale work after revocation or deletion.
        for deleted in [false, true] {
            scheduler.relay_cooldowns.push_back(super::FileHold {
                key,
                until,
                kind: super::FileHoldKind::RelayHeld(Box::new(
                    super::RelayHeldObservation::capture(
                        &node,
                        &destination,
                        &node.routing_peer(&bob.public().user_id()).unwrap(),
                        now,
                    ),
                )),
            });
            if deleted {
                node.delivery
                    .lock()
                    .unwrap()
                    .retire_file_destination(key.0, key.1)
                    .unwrap();
                node.delivery_suspended
                    .store(false, std::sync::atomic::Ordering::Release);
            } else {
                node.delivery_suspended
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            scheduler.admit(&node, vec![(card.clone(), destination.clone())], None);
            assert!(scheduler.transfers.is_empty());
            scheduler.relay_cooldowns.front_mut().unwrap().until = now;
            scheduler.scan(&node);
            super::FILE_RETRY_ROUTE_READS.with(|reads| reads.set(0));
            scheduler.admit(&node, vec![(card.clone(), destination.clone())], None);
            assert_eq!(super::FILE_RETRY_ROUTE_READS.with(std::cell::Cell::get), 1);
            assert!(scheduler.transfers.is_empty());
        }
    }

    #[tokio::test]
    async fn file_scheduler_preserves_candidates_and_clean_same_phase_boundaries() {
        use super::super::test_support::node_with_files;
        use crate::discovery::Announce;
        use crate::identity::{account::Account, device::DeviceIdentity};
        let source = tempfile::tempdir().unwrap();
        let bob = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = Announce::new_with_account(&bob, &account, "Bob", 9);
        let (node, _, _) = node_with_files(
            source.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let path = source.path().join("candidate.bin");
        std::fs::write(&path, b"candidate continuation").unwrap();
        let (id, _) = node
            .enqueue_file_to_account(&account.account_id(), &path, crate::file::FileKind::File)
            .await
            .unwrap();
        let card = node.delivery.lock().unwrap().file_card(id).unwrap().clone();
        let destination = card.destinations[0].clone();
        let key = (id, destination.binding.event_id);
        let routes = node.file_retry_routes(&card, &destination);
        let now = std::time::Instant::now();
        let mut scheduler = super::FileDeliveryScheduler::default();
        let mut diagnostic = super::DeliveryDiagnosticThrottle::default();
        let token = scheduler
            .retries
            .reserve(key, routes.clone(), now)
            .unwrap()
            .0;
        let mut transfer = super::FileTransfer::new(card.clone(), destination.clone());
        transfer.candidate_cursor = 1;
        transfer.candidate_count = 2;
        scheduler.finish_at(
            &node,
            Ok((
                key,
                std::collections::HashMap::from([(key, transfer)]),
                super::FileStepCompletion {
                    token,
                    routes: routes.clone(),
                    outcome: super::FileStepOutcome::Failed,
                },
            )),
            &mut diagnostic,
            now,
        );
        assert!(scheduler.transfers.is_empty());
        assert_eq!(scheduler.parked[&key].candidate_cursor, 1);
        scheduler.admit_at(&node, vec![(card.clone(), destination.clone())], None, now);
        assert!(scheduler.transfers.is_empty());
        let due = now + std::time::Duration::from_secs(2);
        scheduler.admit_at(&node, vec![(card.clone(), destination.clone())], None, due);
        assert_eq!(
            scheduler.transfers[&key].candidate_cursor, 1,
            "retry must retain the next candidate rather than dial the failed first route"
        );
        let token = scheduler
            .retries
            .reserve(key, routes.clone(), due)
            .unwrap()
            .0;
        let mut transfer = scheduler.transfers.remove(&key).unwrap();
        transfer.phase = super::FilePhase::Chunks;
        scheduler.finish_at(
            &node,
            Ok((
                key,
                std::collections::HashMap::from([(key, transfer)]),
                super::FileStepCompletion {
                    token,
                    routes: routes.clone(),
                    outcome: super::FileStepOutcome::Clean(false),
                },
            )),
            &mut diagnostic,
            due,
        );
        assert!(matches!(
            scheduler.transfers[&key].phase,
            super::FilePhase::Chunks
        ));
        assert!(
            scheduler.retries.ready(&key, &routes, due),
            "clean same-phase chunks must continue immediately"
        );
        // A late result for an older route token cannot retire durable intent
        // or publish its stale continuation over the replacement owner.
        let stale = scheduler
            .retries
            .reserve(key, routes.clone(), due)
            .unwrap()
            .0;
        let mut replacement_routes = routes.clone();
        replacement_routes.custody = !replacement_routes.custody;
        let replacement = scheduler
            .retries
            .reserve(key, replacement_routes, due)
            .unwrap()
            .0;
        scheduler.finish_at(
            &node,
            Ok((
                key,
                Default::default(),
                super::FileStepCompletion {
                    token: stale,
                    routes: routes.clone(),
                    outcome: super::FileStepOutcome::Clean(true),
                },
            )),
            &mut diagnostic,
            due,
        );
        assert!(node.file_destination_current(&card, &destination));
        assert!(scheduler.retries.owns(&key, replacement));
        scheduler.retries.complete(&key, replacement, true, due);
        // This injects the scheduler persistence boundary, not crypto acceptance.
        scheduler.transfers.clear();
        let token = scheduler
            .retries
            .reserve(key, routes.clone(), due)
            .unwrap()
            .0;
        node.delivery
            .lock()
            .unwrap()
            .fail_outbox_appends_for_test(true)
            .unwrap();
        assert!(
            node.delivery
                .lock()
                .unwrap()
                .retire_file_destination(key.0, key.1)
                .is_err(),
            "outbox hook must reject actual retirement persistence"
        );
        scheduler.finish_at(
            &node,
            Ok((
                key,
                Default::default(),
                super::FileStepCompletion {
                    token,
                    routes: routes.clone(),
                    outcome: super::FileStepOutcome::Clean(true),
                },
            )),
            &mut diagnostic,
            due,
        );
        assert!(node.file_destination_current(&card, &destination));
        assert!(!scheduler.retries.ready(&key, &routes, due));
        node.delivery
            .lock()
            .unwrap()
            .fail_outbox_appends_for_test(false)
            .unwrap();
        let recovered = due + std::time::Duration::from_secs(2);
        let token = scheduler
            .retries
            .reserve(key, routes.clone(), recovered)
            .unwrap()
            .0;
        scheduler.finish_at(
            &node,
            Ok((
                key,
                Default::default(),
                super::FileStepCompletion {
                    token,
                    routes,
                    outcome: super::FileStepOutcome::Clean(true),
                },
            )),
            &mut diagnostic,
            recovered,
        );
        assert!(!node.file_destination_current(&card, &destination));
    }

    #[tokio::test]
    async fn file_scheduler_new_endpoint_bypasses_failure_but_unrelated_wake_does_not() {
        use super::super::test_support::node_with_files;
        use crate::discovery::Announce;
        use crate::identity::{account::Account, device::DeviceIdentity};
        let source = tempfile::tempdir().unwrap();
        let bob = DeviceIdentity::generate();
        let account = Account::generate();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proof = Announce::new_with_account(
            &bob,
            &account,
            "Bob",
            listener.local_addr().unwrap().port(),
        );
        let (node, _, _) = node_with_files(
            source.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let path = source.path().join("endpoint.bin");
        std::fs::write(&path, b"new endpoint").unwrap();
        let (id, _) = node
            .enqueue_file_to_account(&account.account_id(), &path, crate::file::FileKind::File)
            .await
            .unwrap();
        let card = node.delivery.lock().unwrap().file_card(id).unwrap().clone();
        let destination = card.destinations[0].clone();
        let key = (id, destination.binding.event_id);
        node.roster.lock().unwrap().forget(&proof.public());
        let now = std::time::Instant::now();
        let mut scheduler = super::FileDeliveryScheduler::default();
        let routes = node.file_retry_routes(&card, &destination);
        let token = scheduler
            .retries
            .reserve(key, routes.clone(), now)
            .unwrap()
            .0;
        let mut diagnostic = super::DeliveryDiagnosticThrottle::default();
        scheduler.finish_at(
            &node,
            Ok((
                key,
                Default::default(),
                super::FileStepCompletion {
                    token,
                    routes,
                    outcome: super::FileStepOutcome::Failed,
                },
            )),
            &mut diagnostic,
            now,
        );
        scheduler.admit_at(&node, vec![(card.clone(), destination.clone())], None, now);
        assert!(scheduler.transfers.is_empty());
        node.roster.lock().unwrap().update(
            &proof,
            "127.0.0.1".parse().unwrap(),
            &node.identity.user_id(),
        );
        scheduler.admit_at(&node, vec![(card.clone(), destination.clone())], None, now);
        assert!(scheduler.transfers.contains_key(&key));
        // Pause before polling the admitted worker, then revoke authority.
        scheduler.advance(&node);
        node.delivery_suspended
            .store(true, std::sync::atomic::Ordering::Release);
        while let Some(result) = scheduler.workers.join_next().await {
            scheduler.finish(&node, result, &mut diagnostic);
        }
        assert!(scheduler.transfers.is_empty());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
                .await
                .is_err(),
            "authority revoked before first poll must prevent TCP admission"
        );
        assert!(node.delivery.lock().unwrap().contains_file_destination(
            id,
            key.1,
            &destination.binding.device,
            &destination.binding.account
        ));
    }

    #[tokio::test]
    async fn file_scheduler_expired_inventory_admits_later_durable_destinations() {
        use super::super::test_support::node_with_files;
        use crate::discovery::Announce;
        use crate::identity::{account::Account, device::DeviceIdentity};
        let source = tempfile::tempdir().unwrap();
        let bob = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = Announce::new_with_account(&bob, &account, "Bob", 9);
        let (node, _, _) = node_with_files(
            source.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let mut files = Vec::new();
        for index in 0..=super::MAX_RELAY_CUSTODY_HINTS {
            let path = source.path().join(format!("inventory-{index}.bin"));
            std::fs::write(&path, format!("inventory file {index}")).unwrap();
            let (id, _) = node
                .enqueue_file_to_account(&account.account_id(), &path, crate::file::FileKind::File)
                .await
                .unwrap();
            let card = node.delivery.lock().unwrap().file_card(id).unwrap().clone();
            let destination = card.destinations[0].clone();
            files.push((card, destination));
        }
        let now = std::time::Instant::now();
        let mut scheduler = super::FileDeliveryScheduler::default();
        let mut diagnostic = super::DeliveryDiagnosticThrottle::default();
        for (card, destination) in files.iter().take(super::MAX_RELAY_CUSTODY_HINTS) {
            let key = (card.id, destination.binding.event_id);
            let routes = node.file_retry_routes(card, destination);
            let token = scheduler
                .retries
                .reserve(key, routes.clone(), now)
                .unwrap()
                .0;
            let transfer = super::FileTransfer::new(card.clone(), destination.clone());
            scheduler.finish_at(
                &node,
                Ok((
                    key,
                    std::collections::HashMap::from([(key, transfer)]),
                    super::FileStepCompletion {
                        token,
                        routes,
                        outcome: super::FileStepOutcome::Failed,
                    },
                )),
                &mut diagnostic,
                now,
            );
        }
        assert_eq!(scheduler.parked.len(), super::MAX_RELAY_CUSTODY_HINTS);
        let last = files.last().unwrap().clone();
        let key = (last.0.id, last.1.binding.event_id);
        scheduler.admit_at(&node, vec![last.clone()], Some(key), now);
        scheduler.advance_at(&node, now);
        assert!(
            scheduler.workers.is_empty(),
            "unexpired inventory must not be displaced"
        );
        assert!(scheduler.transfers.is_empty());
        let due = now + std::time::Duration::from_secs(2);
        scheduler.admit_at(&node, vec![last], Some(key), due);
        scheduler.advance_at(&node, due);
        assert!(
            scheduler.running_sockets.contains(&key),
            "expired failed hints must allow the 257th destination to advance"
        );
        assert_eq!(scheduler.parked.len(), super::MAX_RELAY_CUSTODY_HINTS - 1);
        // Cancel before the task is first polled: the admitted retry reservation
        // remains installed while no actual socket or FileCard stays parked.
        scheduler.workers.shutdown().await;
        assert!(!scheduler.retries.ready(
            &key,
            &node.file_retry_routes(&files.last().unwrap().0, &files.last().unwrap().1),
            due
        ));
    }

    #[tokio::test]
    async fn file_scheduler_revoked_inflight_rescan_releases_matching_socket_owner() {
        use super::super::test_support::node_with_files;
        use crate::discovery::Announce;
        use crate::identity::{account::Account, device::DeviceIdentity};
        let source = tempfile::tempdir().unwrap();
        let bob = DeviceIdentity::generate();
        let account = Account::generate();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proof = Announce::new_with_account(
            &bob,
            &account,
            "Bob",
            listener.local_addr().unwrap().port(),
        );
        let (node, _, _) = node_with_files(
            source.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let path = source.path().join("revoked-inflight.bin");
        std::fs::write(&path, b"retained durable work").unwrap();
        let (id, _) = node
            .enqueue_file_to_account(&account.account_id(), &path, crate::file::FileKind::File)
            .await
            .unwrap();
        let card = node.delivery.lock().unwrap().file_card(id).unwrap().clone();
        let destination = card.destinations[0].clone();
        let key = (id, destination.binding.event_id);
        let mut scheduler = super::FileDeliveryScheduler::default();
        let mut diagnostic = super::DeliveryDiagnosticThrottle::default();
        scheduler.admit(&node, vec![(card.clone(), destination.clone())], Some(key));
        scheduler.advance(&node);
        assert!(scheduler.running_sockets.contains(&key));
        assert_eq!(scheduler.workers.len(), 1);
        // The task owns its reservation before its first poll. A journal scan
        // can revisit that durable key while authority is revoked.
        node.delivery_suspended
            .store(true, std::sync::atomic::Ordering::Release);
        let scan = scheduler.scan(&node);
        let (files, cursor) = scan.read(&node.delivery.lock().unwrap());
        assert!(
            files
                .iter()
                .any(|(file, reference)| (file.id, reference.binding.event_id) == key)
        );
        scheduler.admit(&node, files, cursor);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while let Some(result) = scheduler.workers.join_next().await {
                scheduler.finish(&node, result, &mut diagnostic);
            }
        })
        .await
        .expect("revoked worker must finish without networking");
        assert!(
            scheduler.running_sockets.is_empty(),
            "authority rescan must preserve the active token until matching completion releases socket ownership"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
        node.delivery_suspended
            .store(false, std::sync::atomic::Ordering::Release);
        scheduler.admit(&node, vec![(card, destination)], Some(key));
        scheduler.advance(&node);
        assert_eq!(
            scheduler.workers.len(),
            1,
            "restored durable work must be admitted after revoked completion"
        );
        scheduler.workers.shutdown().await;
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

#[cfg(test)]
mod lock_order_tests {
    use std::{sync::Arc, time::Duration};

    #[tokio::test]
    async fn recovery_does_not_hold_received_while_waiting_for_sent() {
        use crate::identity::{account::Account, device::DeviceIdentity};

        let directory = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let proof =
            crate::discovery::Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
        let (node, _messages, _files) = super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let channel = node
            .create_channel("Recovery lock order", vec![])
            .await
            .unwrap();
        // Normal durable acceptance retains a real outgoing WAL transaction when
        // its sent-sidecar installation fails. Recovery must finish that same send.
        node.sentlog
            .lock()
            .unwrap()
            .fail_appends_for_test(true)
            .unwrap();
        let accepted = node
            .enqueue_channel_message_reply_expiring_if(
                channel,
                b"recover this accepted message",
                None,
                None,
                |commit| commit(),
            )
            .unwrap();
        node.sentlog
            .lock()
            .unwrap()
            .fail_appends_for_test(false)
            .unwrap();
        assert_eq!(
            node.delivery.lock().unwrap().pending_transactions().len(),
            1
        );
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        *node.before_recovery_sent_hook.lock().unwrap() = Some(Box::new(move || {
            let _ = entered_tx.try_send(());
        }));
        // Archive snapshots own sent before received. Hold only their first
        // guard, then observe recovery without deliberately forming a lock cycle.
        let sent = node.sentlog.lock().unwrap();
        let worker_node = Arc::clone(&node);
        let worker = std::thread::spawn(move || {
            worker_node.recover_delivery(&mut worker_node.delivery.lock().unwrap())
        });
        let reached_sent = entered_rx.recv_timeout(Duration::from_secs(5));
        let received_available = node.received.try_lock().is_ok();
        drop(sent);
        let recovered = worker.join();
        assert!(reached_sent.is_ok(), "recovery reached the sent boundary");
        assert!(
            received_available,
            "recovery must acquire sent before received"
        );
        assert!(
            recovered.unwrap().is_ok(),
            "recovery completes after release"
        );
        assert!(
            node.delivery
                .lock()
                .unwrap()
                .pending_transactions()
                .is_empty()
        );
        let history = node.channel_history(channel, 10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, accepted);
        assert_eq!(history[0].text, b"recover this accepted message");
    }
}

#[cfg(test)]
mod requester_reception_tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_requester_rounds_dispatch_reverse_dm_already_received_in_response() {
        struct Release(Option<std::sync::mpsc::Sender<()>>);
        impl Drop for Release {
            fn drop(&mut self) {
                if let Some(sender) = self.0.take() {
                    let _ = sender.send(());
                }
            }
        }
        for mode in 0..3 {
            let source_dir = tempfile::tempdir().unwrap();
            let target_dir = tempfile::tempdir().unwrap();
            let source_identity = crate::identity::device::DeviceIdentity::generate();
            let target_identity = crate::identity::device::DeviceIdentity::generate();
            let source_account = crate::identity::account::Account::generate();
            let target_account = crate::identity::account::Account::generate();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let source_proof = crate::discovery::Announce::new_with_account(
                &source_identity,
                &source_account,
                "Source",
                listener.local_addr().unwrap().port(),
            );
            let target_proof = crate::discovery::Announce::new_with_account(
                &target_identity,
                &target_account,
                "Target",
                9,
            );
            let (source, mut bootstrap_incoming, _) = super::super::test_support::node_with_files(
                source_dir.path(),
                source_identity,
                source_account,
                &target_proof,
            );
            let (target, mut incoming, _) = super::super::test_support::node_with_files(
                target_dir.path(),
                target_identity,
                target_account,
                &source_proof,
            );
            let serving = source.clone();
            let server = tokio::spawn(serving.run_accept_loop(listener));
            // Establish the ratchet over the real protocol before exercising reverse
            // reception. Simultaneous first initiations are a separate crypto case.
            let bootstrap = target
                .enqueue_to_account(&source.account_id(), b"establish ratchet", None)
                .await
                .unwrap();
            target.flush_delivery(bootstrap).await;
            let bootstrap =
                tokio::time::timeout(std::time::Duration::from_secs(5), bootstrap_incoming.recv())
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(bootstrap.text, b"establish ratchet");
            let response = source
                .enqueue_to_account(&target.account_id(), b"reverse reply from response", None)
                .await
                .unwrap();
            let response_event = source
                .delivery
                .lock()
                .unwrap()
                .message(response)
                .unwrap()
                .destinations[0]
                .event
                .id;
            let outgoing = target
                .enqueue_to_account(&source.account_id(), b"original outgoing delivery", None)
                .await
                .unwrap();
            let destination = target
                .delivery
                .lock()
                .unwrap()
                .message(outgoing)
                .unwrap()
                .destinations[0]
                .clone();
            let conversation = destination.event.conversation_id;
            let response_cipher = source
                .log
                .lock()
                .unwrap()
                .get(&response_event)
                .unwrap()
                .unwrap()
                .ciphertext;
            assert!(
                target
                    .dm_ratchet
                    .lock()
                    .unwrap()
                    .prepare_decrypt(
                        &target.identity,
                        &source.identity.public(),
                        &response_cipher
                    )
                    .is_ok(),
                "the established reverse packet must already be decryptable without advancing receive state"
            );
            let peer = target.routing_peer(&source.user_id()).unwrap();
            let (entered, ready) = tokio::sync::oneshot::channel();
            let entered = std::sync::Mutex::new(Some(entered));
            let (release, released) = std::sync::mpsc::channel();
            let release = Release(Some(release));
            let released = std::sync::Mutex::new(released);
            let received_phase = format!("event={}", response_event.to_hex());
            *target.account_client_test_hook.lock().unwrap() = Some(Arc::new(move |_, phase| {
                if !phase.starts_with("gossip_diag_ciphertext_applied ")
                    || !phase.ends_with(&received_phase)
                {
                    return;
                }
                if let Some(entered) = entered.lock().unwrap().take() {
                    let _ = entered.send(());
                    let _ = released.lock().unwrap().recv();
                }
            }));
            let receiving = target.clone();
            target.replace_verified_dm_pull_peers(vec![source_proof.user_id.clone()]);
            let request = tokio::spawn(async move {
                loop {
                    match mode {
                        0 => {
                            receiving
                                .transfer_delivery_destination(&peer, &destination, conversation)
                                .await
                        }
                        1 => receiving.deliver_direct(&peer, conversation).await.is_ok(),
                        _ => {
                            receiving
                                .pull_delivery_scope(&source_proof, conversation, false)
                                .await
                                != Reconciliation::Deferred
                        }
                    };
                    // The cancellation boundary is actual ciphertext admission,
                    // not the completion of one possibly deferred range turn.
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), ready)
                .await
                .unwrap()
                .unwrap();
            assert!(
                target
                    .log
                    .lock()
                    .unwrap()
                    .get(&response_event)
                    .unwrap()
                    .is_some()
            );
            assert!(
                !target
                    .received
                    .lock()
                    .unwrap()
                    .processed(response_event)
                    .unwrap()
            );
            request.abort();
            drop(release);
            assert!(request.await.unwrap_err().is_cancelled());
            let received =
                tokio::time::timeout(std::time::Duration::from_secs(3), incoming.recv()).await;
            server.abort();
            let _ = server.await;
            target.runtime_work.close();
            source.runtime_work.close();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                target.runtime_work.drain().await;
                source.runtime_work.drain().await;
            })
            .await
            .expect("actual reception cleanup must retire before fixture storage drops");
            let received = received
            .expect(
                "cancellation must dispatch a reverse DM already durably accepted by the requester",
            )
            .unwrap();
            assert_eq!(received.text, b"reverse reply from response");
            assert!(
                target
                    .received
                    .lock()
                    .unwrap()
                    .processed(response_event)
                    .unwrap()
            );
        }
    }
}

#[cfg(test)]
mod bounded_sync_retry_tests {
    use super::*;

    #[tokio::test]
    async fn control_budget_includes_the_authenticated_preferred_fallback() {
        use crate::{
            discovery::Announce,
            identity::{account::Account, device::DeviceIdentity},
        };
        let dir = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
        let (node, _, _) = super::super::test_support::node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let route = "127.0.0.1:42".parse().unwrap();
        let before = node.control_budget(&peer.public());
        node.preferred_routes
            .lock()
            .unwrap()
            .insert(peer.user_id(), route);
        node.sessions
            .control_succeeded(&peer.public(), route, std::time::Duration::from_secs(12));
        let measured = node.sessions.control_timeout(&peer.public(), route);
        assert!(measured > before);
        assert_eq!(node.control_budget(&peer.public()), measured);
        assert!(measured <= super::super::route_timing::MAX_CONTROL_DEADLINE);
    }

    #[test]
    fn limited_sync_parks_without_claiming_quiet_or_delivery() {
        let now = std::time::Instant::now();
        let mut retries = super::super::delivery_retry::RetrySchedule::default();
        Reconciliation::Limited.schedule(&mut retries, "peer", (), now);
        assert!(!retries.ready(&"peer", &(), now + std::time::Duration::from_secs(1)));
        assert!(retries.ready(&"peer", &(), now + QUIET_REPAIR_INTERVAL));
        assert_eq!(
            Reconciliation::receipt_result(false, false, TransferContinuation::Limited),
            Reconciliation::Limited
        );
        assert_eq!(
            Reconciliation::Limited.combine(Reconciliation::Quiet),
            Reconciliation::Limited
        );
        assert_eq!(
            Reconciliation::Limited.combine(Reconciliation::More),
            Reconciliation::More
        );
    }
    #[test]
    fn incomplete_receipt_is_continuation_without_custody() {
        assert_eq!(
            Reconciliation::receipt_result(false, false, TransferContinuation::More),
            Reconciliation::More
        );
        assert_eq!(
            Reconciliation::receipt_result(true, false, TransferContinuation::More),
            Reconciliation::Advanced
        );
        assert_eq!(
            Reconciliation::receipt_result(false, true, TransferContinuation::More),
            Reconciliation::Deferred
        );
        assert_eq!(
            Reconciliation::receipt_result(false, false, TransferContinuation::None),
            Reconciliation::Deferred
        );
    }
    #[test]
    fn bounded_sync_continuation_resets_failure_backoff_without_claiming_delivery() {
        let now = std::time::Instant::now();
        let mut retries = super::super::delivery_retry::RetrySchedule::default();
        for _ in 0..8 {
            Reconciliation::Deferred.schedule(&mut retries, "peer", (), now);
        }
        Reconciliation::More.schedule(&mut retries, "peer", (), now);
        assert!(!retries.ready(&"peer", &(), now + std::time::Duration::from_millis(499)));
        assert!(retries.ready(&"peer", &(), now + std::time::Duration::from_millis(500)));
        assert_eq!(
            Reconciliation::More.combine(Reconciliation::Quiet),
            Reconciliation::More
        );
        assert_eq!(
            Reconciliation::More.combine(Reconciliation::Deferred),
            Reconciliation::More
        );
        assert_eq!(
            Reconciliation::More.combine(Reconciliation::Advanced),
            Reconciliation::Advanced
        );
    }
}
