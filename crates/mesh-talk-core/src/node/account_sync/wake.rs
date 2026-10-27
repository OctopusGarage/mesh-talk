//! Authenticated sibling invalidation for mandatory and optional account metadata.
//! Category policy still governs every content exchange after this body-free hint.
//! A process epoch distinguishes a restarted publisher's revision sequence.
use super::*;
use rand_core::RngCore;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

const SENDERS: usize = 2;
const DEBOUNCE: Duration = Duration::from_millis(100);
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);
const FAILURE_COOLDOWN: Duration = Duration::from_secs(60);

// A failed local admission exchanged no bytes and says nothing about the peer.
// Keep its revision pending without retrying unchanged work on every loop turn.
struct WakeRetry {
    routes: Vec<std::net::SocketAddr>,
    until: Instant,
    local_revision: Option<u64>,
}
impl WakeRetry {
    fn after_failure(
        routes: Vec<std::net::SocketAddr>,
        revision: u64,
        error: &NodeError,
        now: Instant,
    ) -> Self {
        let local = local_sync_capacity_busy(error);
        Self {
            routes,
            until: now
                + if local {
                    RETRY_INTERVAL
                } else {
                    FAILURE_COOLDOWN
                },
            local_revision: local.then_some(revision),
        }
    }
    fn blocks(&self, routes: &[std::net::SocketAddr], revision: u64, now: Instant) -> bool {
        self.until > now
            && self.routes == routes
            && self.local_revision.is_none_or(|failed| revision <= failed)
    }
}

pub(super) type WakeIdentity = ([u8; 32], [u8; 32]);
pub(super) fn wake_identity(public: &PublicIdentity) -> WakeIdentity {
    (public.ed25519_pub, public.x25519_pub)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ControlWakeHint {
    pub public: PublicIdentity,
    epoch: [u8; 16],
    revision: u64,
}
#[derive(Default)]
struct ReceivedWakes {
    seen: HashMap<WakeIdentity, ([u8; 16], u64)>,
    pending: VecDeque<ControlWakeHint>,
}
impl ReceivedWakes {
    fn retain(&mut self, certified: &HashSet<WakeIdentity>) {
        self.seen.retain(|key, _| certified.contains(key));
        self.pending
            .retain(|hint| certified.contains(&wake_identity(&hint.public)));
    }
    fn record(&mut self, public: PublicIdentity, epoch: [u8; 16], revision: u64) -> bool {
        let key = wake_identity(&public);
        if self
            .seen
            .get(&key)
            .is_some_and(|(old_epoch, old_revision)| {
                *old_epoch == epoch && *old_revision >= revision
            })
        {
            return false;
        }
        if self.seen.get(&key).is_some_and(|(old, _)| *old != epoch) {
            self.pending.retain(|hint| hint.public != public);
        }
        if !self.seen.contains_key(&key) && self.seen.len() >= 64 {
            return false;
        }
        self.seen.insert(key, (epoch, revision));
        let hint = ControlWakeHint {
            public,
            epoch,
            revision,
        };
        if let Some(old) = self
            .pending
            .iter_mut()
            .find(|old| old.public == hint.public)
        {
            *old = hint;
        } else {
            self.pending.push_back(hint);
        }
        true
    }
    fn consume(&mut self, hint: &ControlWakeHint) {
        self.pending.retain(|current| current != hint);
    }
}

pub(in crate::node) struct AccountWake {
    epoch: [u8; 16],
    revision: AtomicU64,
    changed: tokio::sync::Notify,
    // Only exact certified siblings enter this map (the registry is capped at64).
    received: Mutex<ReceivedWakes>,
}
impl Default for AccountWake {
    fn default() -> Self {
        let mut epoch = [0; 16];
        while epoch == [0; 16] {
            rand_core::OsRng.fill_bytes(&mut epoch);
        }
        Self {
            epoch,
            revision: AtomicU64::new(1),
            changed: tokio::sync::Notify::new(),
            received: Mutex::new(ReceivedWakes::default()),
        }
    }
}

impl Node {
    pub(super) fn pending_control_wakes(&self) -> Vec<ControlWakeHint> {
        let own = self.account_id();
        let devices = self.linked_devices();
        let state = self.privacy.state.read().expect("privacy state lock");
        let certified = devices
            .iter()
            .filter(|device| {
                state.as_ref().is_none_or(|state| {
                    state
                        .proofs
                        .account_for(&device.public)
                        .is_none_or(|account| account == own)
                })
            })
            .map(|device| wake_identity(&device.public))
            .collect();
        let mut received = self
            .account_wake
            .received
            .lock()
            .expect("account wake mutex");
        received.retain(&certified);
        received.pending.iter().cloned().collect()
    }
    pub(super) fn consume_control_wake(&self, hint: &ControlWakeHint) {
        self.account_wake
            .received
            .lock()
            .expect("account wake mutex")
            .consume(hint);
    }
    pub(super) fn clear_control_wakes(&self) {
        *self
            .account_wake
            .received
            .lock()
            .expect("account wake mutex") = ReceivedWakes::default();
    }

    /// Local durable publication only. Incoming sibling imports and ordinary pulls
    /// must not call this: doing so would turn a wake/pull into a feedback loop.
    pub(in crate::node) fn note_account_local_change(&self) {
        // Saturation keeps the periodic repair path available without sending an
        // invalid wrapped revision; exhaustion cannot occur in a practical runtime.
        let revision = &self.account_wake.revision;
        let mut current = revision.load(Ordering::Relaxed);
        while let Some(next) = current.checked_add(1) {
            match revision.compare_exchange_weak(
                current,
                next,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
        self.account_wake.changed.notify_one();
    }

    pub(in crate::node) async fn serve_account_wake(
        self: &Arc<Self>,
        channel: &mut SecureChannel<TcpStream>,
        version: AccountSyncVersion,
        account_id: &str,
        epoch: [u8; 16],
        revision: u64,
    ) -> bool {
        {
            let _operation = self.privacy.gate.read().await;
            if version != AccountSyncVersion::V2
                || epoch == [0; 16]
                || revision == 0
                || account_id != self.account_id()
                || !channel.io_admitted()
                || !self.certified_sibling(channel.peer_identity())
                || !self.account_allowed(channel.peer_identity(), channel.peer_announcement())
            {
                return false;
            }
            let certified: HashSet<_> = self
                .linked_devices()
                .iter()
                .map(|device| wake_identity(&device.public))
                .collect();
            let mut received = self
                .account_wake
                .received
                .lock()
                .expect("account wake mutex");
            received.retain(&certified);
            if received.record(channel.peer_identity().clone(), epoch, revision) {
                self.account_sync_notify.notify_one();
            }
        }
        // Duplicate hints still receive an ack. No data or settings cross this path.
        tokio::time::timeout(
            EXCHANGE_TIMEOUT,
            send_frame(
                channel,
                version,
                &AccountHistoryFrame::WakeAck { epoch, revision },
            ),
        )
        .await
        .is_ok_and(|result| result.is_ok())
    }

    pub(super) async fn send_account_wake(
        self: &Arc<Self>,
        peer: &PeerRecord,
        revision: u64,
    ) -> Result<(), NodeError> {
        if peer.account_id.as_deref() != Some(self.account_id().as_str())
            || !self.certified_sibling(&peer.public)
        {
            return Err(NodeError::Authorization(
                "wake destination is not a certified sibling".into(),
            ));
        }
        let epoch = self.account_wake.epoch;
        let mut channel = self
            .begin_account_exchange(
                peer,
                AccountSyncVersion::V2,
                AccountHistoryFrame::Wake {
                    account_id: self.account_id(),
                    epoch,
                    revision,
                },
            )
            .await?;
        let bytes = channel.recv().await.map_err(transport)?;
        match AccountHistoryFrame::decode_versioned(&bytes) {
            Some((
                AccountSyncVersion::V2,
                AccountHistoryFrame::WakeAck {
                    epoch: echoed,
                    revision: echoed_revision,
                },
            )) if echoed == epoch && echoed_revision == revision => {
                channel.recycle(self);
                Ok(())
            }
            _ => Err(NodeError::Channel(
                "sibling wake capability unavailable".into(),
            )),
        }
    }

    pub(in crate::node) async fn run_account_wake_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(RETRY_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut roster = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .subscribe_changes();
        let mut network = self.network_changes.subscribe();
        let mut acknowledged: HashMap<String, u64> = HashMap::new();
        let mut retry_after: HashMap<String, WakeRetry> = HashMap::new();
        let mut active = HashSet::new();
        let mut queue: VecDeque<(String, PeerRecord)> = VecDeque::new();
        let mut senders = tokio::task::JoinSet::new();
        let mut task_owners: HashMap<tokio::task::Id, (String, Vec<std::net::SocketAddr>)> =
            HashMap::new();
        let mut aborts: HashMap<String, tokio::task::AbortHandle> = HashMap::new();
        loop {
            while active.len() < SENDERS {
                let Some((id, peer)) = queue.pop_front() else {
                    break;
                };
                if !active.insert(id.clone()) {
                    continue;
                }
                let Some(permit) = self.runtime_work.admit() else {
                    return;
                };
                let node = self.clone();
                let owner = id.clone();
                let routes = wake_routes(&peer);
                let revision = self.account_wake.revision.load(Ordering::Acquire);
                let task = senders.spawn(permit.track(async move {
                    let result = tokio::time::timeout(
                        EXCHANGE_TIMEOUT,
                        node.send_account_wake(&peer, revision),
                    )
                    .await
                    .unwrap_or_else(|_| Err(NodeError::Channel("sibling wake timed out".into())));
                    (id, revision, result)
                }));
                task_owners.insert(task.id(), (owner.clone(), routes));
                aborts.insert(owner, task);
            }
            tokio::select! {
                _ = tick.tick() => {}
                _ = self.account_wake.changed.notified() => { tokio::time::sleep(DEBOUNCE).await; }
                changed = roster.changed() => {
                    if changed.is_err() { return; }
                    for (id, routes) in task_owners.values() {
                        if self.routing_peer(id).is_none_or(|peer| wake_routes(&peer) != *routes)
                            && let Some(task) = aborts.get(id) { task.abort(); }
                    }
                }
                changed = network.changed() => {
                    if changed.is_err() { return; }
                    senders.shutdown().await; active.clear(); task_owners.clear(); aborts.clear();
                    retry_after.clear(); queue.clear();
                }
                result = senders.join_next_with_id(), if !senders.is_empty() => {
                    match result {
                        Some(Ok((task, (id, revision, result)))) => {
                            let routes = task_owners.remove(&task).map(|(_, routes)| routes).unwrap_or_default();
                            active.remove(&id); aborts.remove(&id);
                            match result {
                                Ok(()) => { acknowledged.insert(id.clone(), revision); retry_after.remove(&id); }
                                Err(error) => { retry_after.insert(id, WakeRetry::after_failure(routes, revision, &error, Instant::now())); }
                            }
                        }
                        Some(Err(error)) => {
                            if let Some((id, routes)) = task_owners.remove(&error.id()) {
                                active.remove(&id); aborts.remove(&id);
                                if !error.is_cancelled() {
                                    log::warn!("Sibling wake worker terminated; periodic reconciliation remains available");
                                    retry_after.insert(id, WakeRetry { routes, until: Instant::now() + FAILURE_COOLDOWN, local_revision: None });
                                }
                            }
                        }
                        None => {}
                    }
                }
            }
            let revision = self.account_wake.revision.load(Ordering::Acquire);
            let devices = self.linked_devices();
            let certified: HashSet<_> = devices
                .iter()
                .map(|device| device.public.user_id())
                .collect();
            acknowledged.retain(|id, _| certified.contains(id));
            retry_after.retain(|id, _| certified.contains(id));
            queue.retain(|(id, _)| certified.contains(id));
            for device in devices {
                let id = device.public.user_id();
                if id == self.user_id()
                    || acknowledged
                        .get(&id)
                        .is_some_and(|known| *known >= revision)
                    || active.contains(&id)
                    || queue.iter().any(|(queued, _)| queued == &id)
                {
                    continue;
                }
                let Some(peer) = self
                    .routing_peer(&id)
                    .filter(|peer| peer.public == device.public)
                else {
                    continue;
                };
                if retry_after.get(&id).is_some_and(|retry| {
                    retry.blocks(&wake_routes(&peer), revision, Instant::now())
                }) {
                    continue;
                }
                queue.push_back((id, peer));
            }
        }
    }
}

fn wake_routes(peer: &PeerRecord) -> Vec<std::net::SocketAddr> {
    let mut routes = peer
        .routes
        .iter()
        .map(|route| route.addr)
        .chain(std::iter::once(peer.addr))
        .collect::<Vec<_>>();
    routes.sort();
    routes.dedup();
    routes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_capacity_retry_requires_a_new_publication_but_remote_failure_keeps_its_cooldown() {
        let now = Instant::now();
        let routes = vec!["127.0.0.1:47474".parse().unwrap()];
        let busy = NodeError::Session(super::super::session::SessionError::Transport(
            crate::transport::TransportError::Noise("TCP connection capacity busy".into()),
        ));
        let local = WakeRetry::after_failure(routes.clone(), 7, &busy, now);
        assert_eq!(local.until, now + RETRY_INTERVAL);
        for _ in 0..64 {
            assert!(
                local.blocks(&routes, 7, now),
                "unchanged busy work must not self-requeue"
            );
        }
        assert!(
            !local.blocks(&routes, 8, now),
            "a real new publication uses the existing Notify"
        );
        assert!(!local.blocks(&routes, 7, now + RETRY_INTERVAL));
        let remote = WakeRetry::after_failure(
            routes.clone(),
            7,
            &NodeError::Channel("sibling wake capability unavailable".into()),
            now,
        );
        assert_eq!(remote.until, now + FAILURE_COOLDOWN);
        assert!(remote.blocks(&routes, 8, now + RETRY_INTERVAL));
        assert!(remote.blocks(&routes, u64::MAX, now));
        assert!(!remote.blocks(&routes, 8, now + FAILURE_COOLDOWN));
        let changed_routes = vec!["127.0.0.1:47475".parse().unwrap()];
        assert!(!local.blocks(&changed_routes, 7, now));
        assert!(!remote.blocks(&changed_routes, 7, now));
    }

    #[test]
    fn control_wake_fifo_coalesces_and_consumes_only_the_captured_generation() {
        use crate::identity::device::DeviceIdentity;
        let a = DeviceIdentity::generate().public();
        let b = DeviceIdentity::generate().public();
        let mut queue = ReceivedWakes::default();
        assert!(queue.record(a.clone(), [1; 16], 1));
        let old = queue.pending[0].clone();
        assert!(queue.record(b.clone(), [1; 16], 1));
        assert!(queue.record(a.clone(), [1; 16], 2));
        queue.consume(&old);
        assert_eq!(queue.pending[0].public, a);
        assert_eq!(queue.pending[0].revision, 2);
        let current = queue.pending[0].clone();
        queue.consume(&current);
        assert_eq!(queue.pending[0].public, b);
        assert!(!queue.record(a.clone(), [1; 16], 2));
        assert!(!queue.record(a.clone(), [1; 16], 1));
        assert_eq!(queue.pending.len(), 1);
        assert!(queue.record(a.clone(), [2; 16], 1));
        queue.consume(&current);
        assert_eq!(queue.pending.back().unwrap().epoch, [2; 16]);
    }

    #[test]
    fn control_wakes_bound_full_identities_and_prune_changed_bindings() {
        use crate::identity::device::DeviceIdentity;
        let a = DeviceIdentity::generate().public();
        let wrong = PublicIdentity {
            ed25519_pub: a.ed25519_pub,
            x25519_pub: DeviceIdentity::generate().public().x25519_pub,
        };
        let mut queue = ReceivedWakes::default();
        assert!(queue.record(a.clone(), [1; 16], 1));
        assert!(queue.record(wrong, [1; 16], 100));
        queue.retain(&HashSet::from([wake_identity(&a)]));
        assert_eq!(queue.pending.len(), 1);
        assert_eq!(queue.pending[0].public, a);
        assert!(queue.record(a, [1; 16], 2));
        for _ in 1..64 {
            assert!(queue.record(DeviceIdentity::generate().public(), [1; 16], 1));
        }
        assert!(!queue.record(DeviceIdentity::generate().public(), [1; 16], 1));
        assert_eq!(queue.pending.len(), 64);
        queue.retain(&HashSet::new());
        assert!(queue.pending.is_empty() && queue.seen.is_empty());
    }

    #[tokio::test]
    async fn imported_signed_sibling_document_notifies_locally_without_originating_wake() {
        use crate::identity::{account::Account, device::DeviceIdentity};
        use crate::node::account_documents::{AccountDocumentKey, DocumentConversation};
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let source_identity = DeviceIdentity::generate();
        let source_public = source_identity.public();
        let source_proof =
            crate::discovery::Announce::new_with_account(&source_identity, &account, "source", 1);
        let source_dir = dir.path().join("source");
        std::fs::create_dir_all(&source_dir).unwrap();
        let (source, _, _) = crate::node::test_support::node_with_files(
            &source_dir,
            source_identity,
            Account::from_secret_bytes(account.secret_bytes()),
            &source_proof,
        );
        let identity = DeviceIdentity::generate();
        let proof =
            crate::discovery::Announce::new_with_account(&identity, &account, "receiver", 1);
        let (node, _, _) =
            crate::node::test_support::node_with_files(dir.path(), identity, account, &proof);
        node.account_devices
            .lock()
            .unwrap()
            .insert(crate::node::account_devices::AccountDevice {
                public: source_public.clone(),
                certificate: node.account.certify(&source_public.ed25519_pub),
            })
            .unwrap();
        let key = AccountDocumentKey::Draft {
            conversation: DocumentConversation::Account("a".repeat(32)),
        };
        let payload = b"signed sibling draft".to_vec();
        let meta = source
            .put_account_document_if(key.clone(), payload.clone(), None, |write| write())
            .unwrap();
        let mut status = initial_sync_status(source.user_id());
        status.document_phase = SyncPhase::UpToDate;
        node.account_sync_status
            .lock()
            .unwrap()
            .insert(source.user_id(), status);
        let documents = node.subscribe_account_documents();
        let ui = node.subscribe_ui_changes();
        let before = node.account_wake.revision.load(Ordering::Acquire);
        let revision = node.device_sync_policy_snapshot().1;
        node.commit_imported_document_for_test(meta, payload.clone(), revision, source_public)
            .await
            .unwrap();
        assert_eq!(
            node.account_document(&key).unwrap().unwrap().payload(),
            payload
        );
        assert!(
            documents.has_changed().unwrap(),
            "import must notify local document readers"
        );
        assert!(ui.has_changed().unwrap(), "import must notify the local UI");
        assert_eq!(
            node.device_sync_status()
                .into_iter()
                .find(|s| s.device_id == source.user_id())
                .unwrap()
                .document_phase,
            SyncPhase::CatchingUp
        );
        assert_eq!(
            node.account_wake.revision.load(Ordering::Acquire),
            before,
            "an incoming sibling row must not originate another publisher wake"
        );
        node.put_account_document_if(
            AccountDocumentKey::Draft {
                conversation: DocumentConversation::Account("b".repeat(32)),
            },
            b"locally authored draft".to_vec(),
            None,
            |write| write(),
        )
        .unwrap();
        assert!(
            node.account_wake.revision.load(Ordering::Acquire) > before,
            "locally authored publication must still wake certified siblings"
        );
    }

    #[test]
    fn local_wake_revisions_are_concurrent_and_saturate_without_wrapping() {
        let dir = tempfile::tempdir().unwrap();
        let identity = crate::identity::device::DeviceIdentity::generate();
        let account = crate::identity::account::Account::generate();
        let proof = crate::discovery::Announce::new_with_account(&identity, &account, "local", 1);
        let (node, _, _) =
            crate::node::test_support::node_with_files(dir.path(), identity, account, &proof);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let node = &node;
                scope.spawn(move || {
                    for _ in 0..100 {
                        node.note_account_local_change();
                    }
                });
            }
        });
        assert_eq!(node.account_wake.revision.load(Ordering::Acquire), 401);
        node.account_wake
            .revision
            .store(u64::MAX - 1, Ordering::Release);
        node.note_account_local_change();
        node.note_account_local_change();
        assert_eq!(node.account_wake.revision.load(Ordering::Acquire), u64::MAX);
    }
}
