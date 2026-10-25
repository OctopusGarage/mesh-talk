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

pub(in crate::node) struct AccountWake {
    epoch: [u8; 16],
    revision: AtomicU64,
    changed: tokio::sync::Notify,
    // Only exact certified siblings enter this map (the registry is capped at64).
    received: Mutex<HashMap<String, ([u8; 16], u64)>>,
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
            received: Mutex::new(HashMap::new()),
        }
    }
}

impl Node {
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
                .into_iter()
                .map(|device| device.public.user_id())
                .collect();
            let mut received = self
                .account_wake
                .received
                .lock()
                .expect("account wake mutex not poisoned");
            received.retain(|id, _| certified.contains(id));
            let id = channel.peer_identity().user_id();
            let fresh = received.get(&id).is_none_or(|(old_epoch, old_revision)| {
                *old_epoch != epoch || *old_revision < revision
            });
            if fresh {
                received.insert(id, (epoch, revision));
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
        let mut retry_after: HashMap<String, (Vec<std::net::SocketAddr>, Instant)> = HashMap::new();
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
                            if result.is_ok() { acknowledged.insert(id.clone(), revision); retry_after.remove(&id); }
                            else { retry_after.insert(id, (routes, Instant::now() + FAILURE_COOLDOWN)); }
                        }
                        Some(Err(error)) => {
                            if let Some((id, routes)) = task_owners.remove(&error.id()) {
                                active.remove(&id); aborts.remove(&id);
                                if !error.is_cancelled() {
                                    log::warn!("Sibling wake worker terminated; periodic reconciliation remains available");
                                    retry_after.insert(id, (routes, Instant::now() + FAILURE_COOLDOWN));
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
                if retry_after.get(&id).is_some_and(|(routes, until)| {
                    *until > Instant::now() && *routes == wake_routes(&peer)
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
