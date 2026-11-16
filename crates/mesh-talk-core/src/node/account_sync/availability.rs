//! Current reachability evidence for background sibling work, never public presence.
use super::*;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::Instant;

type Identity = wake::WakeIdentity;
pub(super) type Scope = (u64, u64, u64);
const EVIDENCE_TTL: Duration = Duration::from_secs(60);
struct Evidence {
    scope: Scope,
    until: Instant,
}
pub(super) struct Availability {
    evidence: Mutex<HashMap<Identity, Evidence>>,
    demand: Mutex<HashSet<Identity>>,
    pub(super) changed: tokio::sync::watch::Sender<u64>,
}
impl Default for Availability {
    fn default() -> Self {
        Self {
            evidence: Default::default(),
            demand: Default::default(),
            changed: tokio::sync::watch::channel(0).0,
        }
    }
}
impl Node {
    /// Coalesced unavailable-to-available transitions, not authentication renewals.
    /// Scheduling evidence only; callers still recheck authority at use.
    pub(in crate::node) fn sibling_availability_revision(&self) -> u64 {
        *self.account_wake.availability.changed.borrow()
    }
    pub(in crate::node) fn subscribe_sibling_availability(
        &self,
    ) -> tokio::sync::watch::Receiver<u64> {
        self.account_wake.availability.changed.subscribe()
    }

    pub(super) fn sibling_availability_scope(&self) -> Scope {
        (
            self.privacy.generation.load(Ordering::Acquire),
            *self.network_changes.borrow(),
            self.device_sync_policy_snapshot().1,
        )
    }
    pub(in crate::node) fn observe_sibling_availability(&self, public: &PublicIdentity) {
        if !self.certified_sibling(public) {
            return;
        }
        self.record_sibling_availability(public, self.sibling_availability_scope(), true);
    }
    pub(super) fn renew_sibling_availability(&self, public: &PublicIdentity, scope: Scope) {
        // Actual successful work may outlive the lease that admitted it, but
        // stale runtime/policy/network completions cannot renew a replacement.
        if self.sibling_availability_scope() == scope && self.certified_sibling(public) {
            self.record_sibling_availability(public, scope, false);
        }
    }
    fn record_sibling_availability(&self, public: &PublicIdentity, scope: Scope, signal: bool) {
        let was_available = self.sibling_sync_available(public);
        let now = Instant::now();
        let certified: HashSet<_> = self
            .linked_devices()
            .iter()
            .map(|device| wake::wake_identity(&device.public))
            .collect();
        let mut evidence = self
            .account_wake
            .availability
            .evidence
            .lock()
            .expect("sibling availability mutex");
        evidence.retain(|identity, value| certified.contains(identity) && value.until > now);
        let identity = wake::wake_identity(public);
        if evidence.get(&identity).is_some_and(|value| {
            value.scope.0 > scope.0 || value.scope.1 > scope.1 || value.scope.2 > scope.2
        }) {
            return;
        }
        let changed = !was_available;
        if evidence.len() < 64 || evidence.contains_key(&identity) {
            evidence.insert(
                identity,
                Evidence {
                    scope,
                    until: now + EVIDENCE_TTL,
                },
            );
        }
        drop(evidence);
        if changed && signal {
            self.request_sibling_pull(public);
            self.account_wake
                .availability
                .changed
                .send_modify(|generation| *generation = generation.wrapping_add(1));
        }
    }
    pub(super) fn sibling_sync_available(&self, public: &PublicIdentity) -> bool {
        self.sibling_availability_deadline(public).is_some()
    }
    pub(super) fn sibling_availability_deadline(&self, public: &PublicIdentity) -> Option<Instant> {
        if !self.certified_sibling(public) || !self.account_allowed(public, None) {
            return None;
        }
        let scope = self.sibling_availability_scope();
        let now = Instant::now();
        let private = self
            .account_wake
            .availability
            .evidence
            .lock()
            .expect("sibling availability mutex")
            .get(&wake::wake_identity(public))
            .filter(|value| value.scope == scope && value.until > now)
            .map(|value| value.until);
        let public = self
            .roster
            .lock()
            .expect("roster lock")
            .get(&public.user_id())
            .filter(|peer| {
                peer.public == *public
                    && peer.account_id.as_deref() == Some(self.account_id().as_str())
            })
            .and_then(|peer| peer.presence_deadline(now));
        private.into_iter().chain(public).max()
    }
    pub(super) fn request_sibling_pull(&self, public: &PublicIdentity) {
        if !self.certified_sibling(public) {
            return;
        }
        let mut demand = self
            .account_wake
            .availability
            .demand
            .lock()
            .expect("sibling demand mutex");
        if demand.len() < 64 {
            demand.insert(wake::wake_identity(public));
        }
        drop(demand);
        self.account_sync_notify.notify_one();
    }
    /// Exercise the real receiver-owned demand without manufacturing a publisher Wake.
    #[cfg(test)]
    pub(in crate::node) fn request_sibling_pull_for_test(&self, public: &PublicIdentity) {
        self.request_sibling_pull(public);
    }
    pub(super) fn take_sibling_pull_requests(&self) -> HashSet<Identity> {
        std::mem::take(
            &mut *self
                .account_wake
                .availability
                .demand
                .lock()
                .expect("sibling demand mutex"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn private_evidence_coalesces_and_expires_without_public_presence() {
        let (_dir, source, receiver) = super::super::selection_tests::sibling_nodes();
        let mut changes = receiver.account_wake.availability.changed.subscribe();
        let public = source.identity.public();
        assert!(!receiver.sibling_sync_available(&public));
        receiver.observe_sibling_availability(&public);
        assert!(receiver.sibling_sync_available(&public));
        assert!(changes.has_changed().unwrap());
        changes.borrow_and_update();
        receiver.take_sibling_pull_requests();
        receiver.observe_sibling_availability(&public);
        assert!(
            !changes.has_changed().unwrap(),
            "authentication heartbeat must coalesce"
        );
        assert!(receiver.take_sibling_pull_requests().is_empty());
        assert!(receiver.roster.lock().unwrap().peers().is_empty());
        receiver
            .account_wake
            .availability
            .evidence
            .lock()
            .unwrap()
            .get_mut(&wake::wake_identity(&public))
            .unwrap()
            .until = Instant::now() - Duration::from_secs(1);
        assert!(!receiver.sibling_sync_available(&public));
        receiver.observe_sibling_availability(&public);
        assert!(changes.has_changed().unwrap());
        assert!(
            receiver
                .take_sibling_pull_requests()
                .contains(&wake::wake_identity(&public))
        );
    }

    #[tokio::test]
    async fn private_evidence_rejects_network_policy_and_identity_replacement() {
        let (_dir, source, receiver) = super::super::selection_tests::sibling_nodes();
        let public = source.identity.public();
        receiver.observe_sibling_availability(&public);
        let mut changed_identity = public.clone();
        changed_identity.x25519_pub[0] ^= 1;
        assert!(!receiver.sibling_sync_available(&changed_identity));
        receiver
            .network_changes
            .send_modify(|generation| *generation += 1);
        assert!(!receiver.sibling_sync_available(&public));
        receiver.observe_sibling_availability(&public);
        assert!(receiver.sibling_sync_available(&public));
        receiver
            .set_device_sync_category(SyncCategory::Attachments, true)
            .unwrap();
        assert!(!receiver.sibling_sync_available(&public));
        let mut status = initial_sync_status(source.user_id());
        status.phase = SyncPhase::Error;
        status.pending_attachments = 9;
        status.error = Some("old transport failure".into());
        let legacy = receiver.subscribe_ui_changes();
        let desktop = receiver.subscribe_desktop_ui_changes();
        let desktop_before = *desktop.borrow();
        receiver.set_account_sync_status(source.user_id(), status.clone());
        assert!(
            legacy.has_changed().unwrap(),
            "status commits must retain legacy worker wakes"
        );
        let desktop_after = *desktop.borrow();
        assert_eq!(
            desktop_after.state, desktop_before.state,
            "a status-only commit must not invalidate desktop group state"
        );
        assert_eq!(desktop_after.documents, desktop_before.documents);
        assert!(
            desktop_after.sync_status > desktop_before.sync_status,
            "the actual status setter must invalidate status consumers"
        );
        receiver.set_account_sync_status(source.user_id(), status);
        assert_eq!(
            *desktop.borrow(),
            desktop_after,
            "an unchanged status must remain a notification no-op"
        );
        let status = receiver.device_sync_status().remove(0);
        assert_eq!(status.phase, SyncPhase::Offline);
        assert_eq!(status.pending_attachments, 9);
        assert!(status.error.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn continuous_same_endpoint_heartbeats_do_not_restart_history_after_original_lease() {
        use futures_util::FutureExt;
        use std::sync::atomic::{AtomicBool, AtomicUsize};

        let (_dir, source, receiver) = super::super::selection_tests::sibling_nodes();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let announce = source.signed_announce("source", address.port());
        receiver
            .roster
            .lock()
            .unwrap()
            .update_routed(&announce, address.ip(), &receiver.user_id());
        let attempts = Arc::new(AtomicUsize::new(0));
        let observed = Arc::new(AtomicUsize::new(0));
        let idle = Arc::new(AtomicBool::new(false));
        *receiver.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let attempts = attempts.clone();
            let observed = observed.clone();
            let idle = idle.clone();
            move |_, phase| {
                if phase.starts_with("history_begin ") {
                    attempts.fetch_add(1, Ordering::SeqCst);
                }
                if phase == "account_owner_roster_observed" {
                    idle.store(false, Ordering::SeqCst);
                    observed.fetch_add(1, Ordering::SeqCst);
                }
                if let Some(state) = phase.strip_prefix("account_owner_local_state idle=") {
                    idle.store(state == "true", Ordering::SeqCst);
                }
            }
        }));
        let server = tokio::spawn(source.clone().run_accept_loop(listener));
        let worker = tokio::spawn(receiver.clone().run_account_history_loop_with_tick(None));
        // Keep fixture retirement outside assertions, including failed setup.
        let result = std::panic::AssertUnwindSafe(async {
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    if idle.load(Ordering::SeqCst)
                        && receiver.device_sync_status().iter().any(|status| {
                            status.device_id == source.user_id()
                                && status.phase == SyncPhase::UpToDate
                                && status.document_phase == SyncPhase::UpToDate
                        })
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("actual startup history/documents and local owner must settle");
            let before = attempts.load(Ordering::SeqCst);
            assert!(before > 0, "the real sibling history worker must have run");
            let original_deadline = receiver
                .sibling_availability_deadline(&source.identity.public())
                .expect("the real source is available before measurement");
            let window = EVIDENCE_TTL.max(crate::discovery::roster::PUBLIC_PRESENCE_TTL)
                + Duration::from_secs(5);
            let end = Instant::now() + window;
            let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // Cross the original real std::Instant deadline while every actual
            // roster observation keeps the source continuously available.
            loop {
                heartbeat.tick().await;
                assert!(
                    receiver.sibling_sync_available(&source.identity.public()),
                    "this control must not contain a genuine availability lapse"
                );
                let next = observed.load(Ordering::SeqCst) + 1;
                assert!(
                    receiver
                        .roster
                        .lock()
                        .unwrap()
                        .update_routed(&announce, address.ip(), &receiver.user_id(),)
                        .accepted()
                );
                tokio::time::timeout(Duration::from_secs(3), async {
                    while observed.load(Ordering::SeqCst) < next || !idle.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("the owner must consume this heartbeat before the assertion");
                assert_eq!(
                    attempts.load(Ordering::SeqCst),
                    before,
                    "continuous identical presence must not manufacture reconnect history work"
                );
                assert!(
                    !worker.is_finished(),
                    "the account owner must remain running"
                );
                if Instant::now() >= end {
                    break;
                }
            }
            assert!(Instant::now() > original_deadline);
            assert!(receiver.sibling_sync_available(&source.identity.public()));
        })
        .catch_unwind()
        .await;
        worker.abort();
        server.abort();
        let _ = worker.await;
        let _ = server.await;
        *receiver.account_client_test_hook.lock().unwrap() = None;
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refreshed_same_endpoint_after_public_expiry_pulls_without_tick() {
        let (_dir, source, receiver) = super::super::selection_tests::sibling_nodes();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let announce = source.signed_announce("source", address.port());
        receiver
            .roster
            .lock()
            .unwrap()
            .update_routed(&announce, address.ip(), &receiver.user_id());
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        *receiver.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let attempts = attempts.clone();
            move |_, phase| {
                if phase.starts_with("history_begin ") {
                    attempts.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
        let server = tokio::spawn(source.clone().run_accept_loop(listener));
        let worker = tokio::spawn(receiver.clone().run_account_history_loop_with_tick(None));
        tokio::time::timeout(Duration::from_secs(10), async {
            while !receiver.device_sync_status().iter().any(|status| {
                status.phase == SyncPhase::UpToDate && status.document_phase == SyncPhase::UpToDate
            }) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let before = attempts.load(Ordering::SeqCst);
        for evidence in receiver
            .account_wake
            .availability
            .evidence
            .lock()
            .unwrap()
            .values_mut()
        {
            evidence.until = Instant::now() - Duration::from_secs(1);
        }
        tokio::time::sleep(
            crate::discovery::roster::PUBLIC_PRESENCE_TTL + Duration::from_millis(100),
        )
        .await;
        assert!(!receiver.sibling_sync_available(&source.identity.public()));
        assert!(receiver.routing_peer(&source.user_id()).is_some());
        receiver
            .roster
            .lock()
            .unwrap()
            .update_routed(&announce, address.ip(), &receiver.user_id());
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            while attempts.load(Ordering::SeqCst) == before {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        worker.abort();
        server.abort();
        let _ = worker.await;
        let _ = server.await;
        result.expect(
            "expired then refreshed evidence at the same endpoint must schedule immediately",
        );
    }
}
