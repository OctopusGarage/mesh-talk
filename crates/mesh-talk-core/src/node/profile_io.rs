//! Publishing our own avatar as a signed profile, and receiving peers' profiles. The
//! profile rides the SAME reliable per-device-pair sync as account DMs: it's sealed to
//! each peer device, appended as an `EventKind::Profile` event, delivered direct, and
//! replicated to the post office — so it converges like a message, not via a fragile
//! one-shot push. Split out of `node.rs` (one `impl Node` block per domain).

use super::*;
use crate::discovery::roster::PeerRecord;
use crate::eventlog::event::{Author, ConversationId, Event, EventId, EventKind};
use crate::eventlog::{LogError, PersistentEventLog};
use crate::identity::device::PublicIdentity;
use crate::node::conversation::dm_conversation_id;
use crate::node::profile::ProfilePayload;
use std::sync::Arc;
use std::time::Duration;

/// Each pass reclaims at most one conversation; the persistent log also applies a cooldown.
const PROFILE_COMPACTION_INTERVAL: Duration = Duration::from_secs(3);

const PROFILE_REPAIR_INTERVAL: Duration = Duration::from_secs(300);
struct ProfileRetrySweep {
    upper: Option<(i64, EventId)>,
    after: (i64, EventId),
    finished: bool,
    contended: bool,
    resweep: bool,
}
impl Default for ProfileRetrySweep {
    fn default() -> Self {
        Self {
            upper: None,
            after: (0, EventId::new([0; 32])),
            finished: false,
            contended: false,
            resweep: false,
        }
    }
}
enum ProfileTurn {
    Continue,
    Blocked(tokio::time::Instant),
    Settled,
    Retry,
}

/// Optional sender provenance at the existing local event-allocation owner.
pub(super) struct LocalProfilePublication<'a> {
    pub required_parent: Option<EventId>,
    pub payload: Option<(&'a PublicIdentity, &'a [u8])>,
}

impl Node {
    pub(super) fn profile_publication_key(
        &self,
        peer: &PublicIdentity,
        wire: &[u8],
        parent: Option<EventId>,
    ) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(b"mesh-talk-local-profile-publication-v1");
        hash.update(self.account.public().ed25519_pub);
        let own = self.identity.public();
        for key in [
            own.ed25519_pub,
            own.x25519_pub,
            peer.ed25519_pub,
            peer.x25519_pub,
        ] {
            hash.update(key);
        }
        hash.update([u8::from(parent.is_some())]);
        if let Some(parent) = parent {
            hash.update(parent.as_bytes());
        }
        hash.update((wire.len() as u64).to_be_bytes());
        hash.update(wire);
        hash.finalize().into()
    }
    pub(super) fn retained_profile_publication(
        &self,
        log: &PersistentEventLog,
        key: &[u8; 32],
        conversation: ConversationId,
        required_parent: Option<EventId>,
        independent: bool,
    ) -> Result<Option<(u64, EventId)>, NodeError> {
        let Some(event) = log.retained_profile_publication(key)? else {
            return Ok(None);
        };
        if event.kind != EventKind::Profile
            || event.conversation_id != conversation
            || event.author.ed25519_pub() != &self.identity.public().ed25519_pub
            || required_parent.is_some_and(|parent| !event.parents.contains(&parent))
            || (independent
                && event
                    .parents
                    .iter()
                    .any(|parent| Some(*parent) != required_parent))
        {
            return Ok(None);
        }
        Ok(Some((event.seq, event.id)))
    }

    /// Set (or clear) this user's OWN avatar and PROPAGATE it to peers. `avatar` is the
    /// small data-URL bytes the UI produces (or `None` to clear). A SET larger than
    /// [`crate::node::MAX_AVATAR_BYTES`] is rejected. Builds a signed [`ProfilePayload`] (authenticated
    /// by the account key), records it as our current profile (so a freshly-discovered
    /// peer gets re-published the same version), then seals + appends + delivers one copy
    /// to every known peer device + our own other devices (the account fan-out set), so
    /// contacts actually see the photo. Best-effort delivery per device (the event is
    /// durably logged and syncs later if a peer is offline).
    pub async fn set_avatar(&self, avatar: Option<Vec<u8>>) -> Result<(), NodeError> {
        self.set_avatar_if(avatar, |commit| commit()).await
    }

    pub async fn set_avatar_if(
        &self,
        avatar: Option<Vec<u8>>,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        let mut delivery = Vec::new();
        authorize(&mut || {
            let mut store = self.profiles.lock().expect("profiles mutex not poisoned");
            if let Some(mut profile) = store.get(&self.account_id())? {
                if profile.avatar == avatar {
                    // Repair a previous committed intent before acknowledging a retry.
                    self.project_account_profile(&store, &profile)?;
                    return Ok(());
                }
                let last = profile.revision.max(
                    self.account_profiles
                        .current()
                        .map_or(0, |value| value.updated_at),
                );
                profile.revision = last
                    .checked_add(1)
                    .ok_or_else(|| {
                        NodeError::InvalidInput("account profile revision exhausted".into())
                    })?
                    .max(super::node::now_millis());
                profile.avatar = avatar.clone();
                profile.resign(&self.account);
                if !store.record(profile.clone()).map_err(NodeError::Log)? {
                    return Err(NodeError::InvalidInput(
                        "invalid or over-budget account profile".into(),
                    ));
                }
                self.project_account_profile(&store, &profile)?;
                // Keep the durable edit atomic, then release before channel/log work.
                drop(store);
                for peer in self.profile_fanout_targets() {
                    if let Some(conv) = self.prepare_account_profile_delivery(&peer, &profile) {
                        delivery.push((peer, conv));
                    }
                }
            } else if let Some(payload) = self.account_profiles.set_avatar(avatar.clone())? {
                drop(store);
                for peer in self.profile_fanout_targets() {
                    if let Some(conv) = self.prepare_profile_delivery(&peer, &payload.encode()) {
                        delivery.push((peer, conv));
                    }
                }
            }
            Ok(())
        })?;
        for (peer, conv) in delivery {
            let _ = self.deliver_direct(&peer, conv).await;
            let _ = self.replicate_to_post_office(conv).await;
        }
        Ok(())
    }

    /// Revision of the current public profile, including an avatar clear.
    pub(in crate::node) fn public_profile_revision(&self) -> Result<Option<u64>, LogError> {
        let card = self
            .profiles
            .lock()
            .expect("profiles mutex not poisoned")
            .version(&self.account_id())?;
        let avatar = self
            .account_profiles
            .current()
            .map(|profile| profile.updated_at);
        Ok(card.into_iter().chain(avatar).max())
    }

    /// Report whether direct reconciliation or relay publication completed, so
    /// a transient failure remains eligible for a later runtime poll.
    pub(in crate::node) async fn deliver_profile_to(
        &self,
        peer: &PeerRecord,
        conv: ConversationId,
    ) -> bool {
        let direct = async {
            let delivered = self
                .adaptive_control_timeout(&peer.public, self.deliver_direct(peer, conv))
                .await
                .is_some_and(|result| result.is_ok());
            if delivered {
                // Reconciliation can pull a newer sibling profile while pushing
                // ours. Apply it before recording this publication as complete.
                self.process_profile_events(conv);
            }
            delivered
        };
        let relay = async {
            tokio::time::timeout(
                super::route_timing::MAX_CONTROL_DEADLINE,
                self.replicate_to_post_office(conv),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(false)
        };
        tokio::pin!(direct, relay);
        // Either successful path completes this publication. Dropping the other
        // future closes its channel; retries reconstruct any round-local state.
        tokio::select! {
            delivered = &mut direct => delivered || relay.await,
            published = &mut relay => published || direct.await,
        }
    }

    /// Every distinct peer DEVICE to publish a profile to: every known peer (any account)
    /// plus our own other devices, never ourselves, deduped by user-id. Unlike a DM
    /// fan-out we don't require a specific target account — a profile goes to everyone we
    /// know so any contact can render it.
    pub(super) fn profile_fanout_targets(&self) -> Vec<PeerRecord> {
        let me = self.identity.public().user_id();
        let mut seen = std::collections::HashSet::new();
        self.routing_peers()
            .into_iter()
            .filter(|p| !p.post_office) // a post office relays, it doesn't render avatars
            .filter(|p| p.public.user_id() != me)
            .filter(|p| seen.insert(p.public.user_id()))
            .collect()
    }

    /// Shared synchronous acceptance policy for user publication and background replay.
    pub(super) fn prepare_profile_delivery(
        &self,
        peer: &PeerRecord,
        wire: &[u8],
    ) -> Option<ConversationId> {
        self.prepare_profile_delivery_with_parent(peer, wire, None)
            .map(|(conv, _)| conv)
    }

    pub(super) fn prepare_profile_delivery_with_parent(
        &self,
        peer: &PeerRecord,
        wire: &[u8],
        required_parent: Option<crate::eventlog::event::EventId>,
    ) -> Option<(ConversationId, crate::eventlog::event::EventId)> {
        #[cfg(test)]
        if let Some(hook) = self.profile_prepare_hook.lock().unwrap().take() {
            hook();
        }
        if !self.known_account_allowed(&peer.public) {
            return None;
        }
        let conv = dm_conversation_id(&self.identity.public(), &peer.public);
        let (_, id) = self
            .append_profile_publication(&peer.public, wire, required_parent)
            .ok()?;
        Some((conv, id))
    }

    /// Receive a bounded metadata discovery/retry page. Unknown devices and
    /// temporarily disabled sibling synchronization keep durable pending work.
    pub(in crate::node) fn process_profile_events(&self, conv: ConversationId) {
        if let Err(error) = self.drain_profile_receptions(Some(conv)) {
            log::warn!("profile reception deferred after storage failure: {error}");
        }
    }

    fn drain_profile_receptions(
        &self,
        conversation: Option<ConversationId>,
    ) -> Result<bool, NodeError> {
        self.drain_profile_receptions_turn(conversation, None)
    }
    fn drain_profile_receptions_turn(
        &self,
        conversation: Option<ConversationId>,
        mut sweep: Option<&mut ProfileRetrySweep>,
    ) -> Result<bool, NodeError> {
        if let Some(sweep) = sweep.as_deref_mut() {
            sweep.contended = true;
        }
        let work = match self.profile_receive_work.try_lock() {
            Ok(work) => work,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(false),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(NodeError::Log(crate::eventlog::LogError::CorruptFile(
                    "profile receive owner unavailable".into(),
                )));
            }
        };
        if let Some(sweep) = sweep.as_deref_mut() {
            sweep.contended = false;
        }
        let after = work.after()?;
        let headers = self
            .log
            .lock()
            .expect("log mutex not poisoned")
            .repository()?
            .committed_headers_page(after, 128)?;
        let discovering = headers.len() == 128;
        // Commit raw maintenance demand before acknowledging discovery. A failed
        // cursor commit retries this idempotent bounded metadata batch.
        self.log
            .lock()
            .expect("log mutex not poisoned")
            .repository()?
            .request_discovered_profile_maintenance(&headers)?;
        work.discover(
            after,
            &headers,
            Author::from_ed25519(self.identity.public().ed25519_pub),
        )?;
        if let Some(sweep) = sweep {
            if !sweep.finished {
                if sweep.upper.is_none() {
                    sweep.upper = work.retry_upper()?;
                }
                let page = match sweep.upper {
                    Some(upper) => work.retry_page(sweep.after, upper)?,
                    None => Vec::new(),
                };
                let short = page.len() < 16;
                for (ticket, candidate) in page {
                    self.process_profile_reception_candidate(&work, candidate)?;
                    sweep.after = ticket;
                }
                sweep.finished = short || sweep.upper.is_none_or(|upper| sweep.after >= upper);
            }
            if sweep.finished {
                let latest = work.retry_upper()?;
                if sweep.resweep {
                    // Concurrent dependency changes earn one new finite sweep,
                    // after the active cursor reaches its captured cut.
                    sweep.after = (0, EventId::new([0; 32]));
                    sweep.upper = latest;
                    sweep.finished = latest.is_none();
                    sweep.resweep = false;
                } else if latest > sweep.upper {
                    // Discovery can commit later tickets after this finite cut.
                    // Start a bounded suffix without rescanning blocked prefixes.
                    sweep.after = sweep.upper.unwrap_or((0, EventId::new([0; 32])));
                    sweep.upper = latest;
                    sweep.finished = false;
                }
            }
        } else {
            for candidate in work.candidates_for(conversation)? {
                self.process_profile_reception_candidate(&work, candidate)?;
            }
        }
        Ok(discovering)
    }

    fn process_profile_reception_candidate(
        &self,
        work: &crate::storage::profile::profile_receive::ProfileReceiveWork,
        candidate: crate::storage::profile::profile_receive::Candidate,
    ) -> Result<(), NodeError> {
        // Read retained metadata under a short guard. Routing and profile
        // owners must never be entered while holding the event-log guard.
        let header = {
            self.log
                .lock()
                .expect("log mutex not poisoned")
                .repository()?
                .header(&candidate.id)?
        };
        if let Some(header) = header {
            if header.conversation != candidate.conversation || header.kind != EventKind::Profile {
                return Err(NodeError::Log(LogError::CorruptFile(
                    "profile retry source changed".into(),
                )));
            }
            let Some(peer) = self.routing_peer(&header.author.user_id()) else {
                return Ok(());
            };
            if peer.account_id.as_deref() == Some(self.account_id().as_str())
                && !self.device_sync_policy().profile
            {
                return Ok(());
            }
        } else {
            work.complete(candidate.id)?;
            self.note_reception_change();
            return Ok(());
        }
        let event = self
            .log
            .lock()
            .expect("log mutex not poisoned")
            .get(&candidate.id)?;
        let complete = match event {
            None => true, // a retired source no longer owns presentation work
            Some(event) if event.conversation_id == candidate.conversation => {
                self.process_profile_candidate(&event)?
            }
            Some(_) => {
                return Err(NodeError::Log(crate::eventlog::LogError::CorruptFile(
                    "profile retry source changed".into(),
                )));
            }
        };
        if complete {
            work.complete(candidate.id)?;
            self.note_reception_change();
        }

        Ok(())
    }

    fn process_profile_candidate(&self, event: &Event) -> Result<bool, NodeError> {
        let Some(peer) = self.routing_peer(&event.author.user_id()) else {
            return Ok(false);
        };
        let Ok(plaintext) =
            crate::dm::open(&self.identity, &peer.public.x25519_pub, &event.ciphertext)
        else {
            return Ok(false);
        };
        if plaintext.starts_with(b"MTP3") {
            let Some(profile) = AccountProfile::decode(&plaintext) else {
                return Ok(true);
            };
            if peer.account_id.as_deref() != Some(profile.account_id().as_str()) {
                return Ok(true);
            }
            if profile.account_id() == self.account_id() && !self.device_sync_policy().profile {
                return Ok(false);
            }
            self.receive_account_profile(&profile)?;
            return Ok(true);
        }
        if plaintext.starts_with(b"MTC1") {
            let Some(card) = TrustCard::decode(&plaintext) else {
                return Ok(true);
            };
            if peer.account_id.as_deref() != Some(card.account_id().as_str()) {
                return Ok(true);
            }
            if card.account_id() == self.account_id() && !self.device_sync_policy().profile {
                return Ok(false);
            }
            self.receive_trust_card(&card)?;
            return Ok(true);
        }
        let Some(payload) = ProfilePayload::decode(&plaintext) else {
            return Ok(true);
        };
        if !payload.verify() || peer.account_id.as_deref() != Some(payload.account_id().as_str()) {
            return Ok(true);
        }
        if payload.account_id() == self.account_id() && !self.device_sync_policy().profile {
            return Ok(false);
        }
        if self.receive_legacy_profile(&payload)? {
            let _ = self.profile_incoming.send(ReceivedProfile {
                account_id: payload.account_id(),
                avatar: payload.avatar,
            });
        }
        Ok(true)
    }

    /// Drain one deferred profile compaction without holding any async/network state.
    pub(in crate::node) fn drain_profile_compaction(
        &self,
    ) -> Result<Option<usize>, crate::eventlog::LogError> {
        self.drain_profile_storage().map(|(dropped, _)| dropped)
    }
    fn drain_profile_storage(&self) -> Result<(Option<usize>, bool), LogError> {
        // Watchdog retries may retain a bounded repair lease between exchanges.
        // Revoked or abandoned jobs must not indefinitely pin Profile storage.
        self.opaque_upload_cache
            .lock()
            .expect("upload cache")
            .clear_stale_receive_repair_leases(
                self.privacy
                    .generation
                    .load(std::sync::atomic::Ordering::Acquire),
            );
        self.storage.maintain_participant_events()?;
        let cleanup_more = self
            .log
            .lock()
            .expect("log mutex not poisoned")
            .repository()?
            .cleanup_selections()?;
        // A causal repair keeps recovered Profile parents available until its
        // child has crossed admission. Serialize only the synchronous physical
        // compaction with lease acquisition; no database guard spans network I/O.
        let leases = self.profile_compaction_leases.lock().map_err(|_| {
            LogError::CorruptFile("profile compaction lease owner unavailable".into())
        })?;
        if *leases != 0 {
            return Ok((None, cleanup_more));
        }
        self.log
            .lock()
            .expect("log mutex not poisoned")
            .drain_one_profile_compaction()
            .map(|dropped| (dropped, cleanup_more))
    }

    fn profile_dependency_input(
        &self,
    ) -> (
        Vec<crate::discovery::Announce>,
        Vec<(
            PublicIdentity,
            crate::discovery::roster::SchedulingEndpoints,
        )>,
        u64,
    ) {
        // Capture the complete cache input under one short in-memory guard.
        // Signed facts and canonical endpoints exclude lease/preference churn.
        let roster = self.roster.lock().expect("roster lock");
        let mut announcements = roster.announcements();
        announcements.sort_by_key(|proof| (proof.ed25519_pub, proof.x25519_pub));
        let mut routes: Vec<_> = roster
            .peers()
            .into_iter()
            .map(|peer| {
                let endpoints = peer.scheduling_endpoints();
                (peer.public, endpoints)
            })
            .collect();
        routes.sort_by_key(|peer| (peer.0.ed25519_pub, peer.0.x25519_pub));
        let generation = self
            .privacy
            .generation
            .load(std::sync::atomic::Ordering::Acquire);
        (announcements, routes, generation)
    }

    fn profile_maintenance_turn(
        &self,
        sweep: &mut ProfileRetrySweep,
    ) -> Result<ProfileTurn, NodeError> {
        self.cache_discovered_peers()
            .map_err(|error| NodeError::Log(LogError::Io(error)))?;
        self.observe_order_work();
        let discovering = self.drain_profile_receptions_turn(None, Some(sweep))?;
        let before = self
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .observe_profile_work()?;
        let dropped = self.drain_profile_compaction()?;
        let cleanup_more = self
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .selection_cleanup_pending()?;
        let (staging, archive) = self.storage.participant_event_maintenance_pending()?;
        let after = self
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .observe_profile_work()?;
        let deadline = self
            .opaque_upload_cache
            .lock()
            .expect("upload cache")
            .repair_maintenance_deadline(
                self.privacy
                    .generation
                    .load(std::sync::atomic::Ordering::Acquire),
            );
        if sweep.contended || archive {
            // Reception contention must not defer independent storage cleanup or
            // expiry of an abandoned repair lease. Retry the receive owner at its
            // existing cadence, bounded by any earlier owned repair deadline.
            return Ok(ProfileTurn::Blocked(
                deadline
                    .unwrap_or(tokio::time::Instant::now() + PROFILE_COMPACTION_INTERVAL)
                    .min(tokio::time::Instant::now() + PROFILE_COMPACTION_INTERVAL),
            ));
        }
        if discovering
            || !sweep.finished
            || cleanup_more
            || staging
            || dropped.is_some_and(|count| count > 0)
        {
            return Ok(ProfileTurn::Continue);
        }
        if let Some(deadline) = deadline {
            return Ok(ProfileTurn::Blocked(deadline));
        }
        let leases = *self.profile_compaction_leases.lock().map_err(|_| {
            NodeError::Log(LogError::CorruptFile(
                "profile compaction lease owner unavailable".into(),
            ))
        })?;
        if leases > 0 {
            return Ok(ProfileTurn::Blocked(
                tokio::time::Instant::now() + PROFILE_REPAIR_INTERVAL,
            ));
        }
        if after.job.as_ref().is_some_and(|job| !job.complete) {
            return Ok(if before != after {
                ProfileTurn::Continue
            } else {
                ProfileTurn::Retry
            });
        }
        let pending = self
            .profile_receive_work
            .lock()
            .expect("profile receive owner")
            .retry_upper()?
            .is_some();
        if pending || after.requests_pending {
            Ok(ProfileTurn::Blocked(
                tokio::time::Instant::now() + PROFILE_REPAIR_INTERVAL,
            ))
        } else {
            Ok(ProfileTurn::Settled)
        }
    }

    /// One accept-loop-owned scheduler; subscriptions precede initial work and
    /// their captured observations retire before the admitted worker starts.
    pub(in crate::node) async fn run_profile_compaction_loop(self: Arc<Self>) {
        let mut source = self.storage.database().subscribe_source_changes();
        let mut maintenance = self.storage.database().subscribe_maintenance_changes();
        let mut roster = self.subscribe_roster_changes();
        let mut ui = self.subscribe_ui_changes();
        let mut network = self.network_changes.subscribe();
        let mut input = self.profile_dependency_input();
        let mut deadline = tokio::time::Instant::now();
        let mut continuation = false;
        let mut sweep = ProfileRetrySweep::default();
        loop {
            let demand = tokio::select! {
                _ = tokio::time::sleep_until(deadline) => !continuation,
                changed = source.changed() => { if changed.is_err() { return; } true },
                changed = maintenance.changed() => { if changed.is_err() { return; } true },
                changed = network.changed() => { if changed.is_err() { return; } true },
                changed = roster.changed() => {
                    if changed.is_err() { return; }
                    let next = self.profile_dependency_input();
                    #[cfg(test)]
                    self.trace_account_client(&self.identity.public(), || "profile_maintenance_roster_observed".into());
                    if next == input { continue; }
                    true
                },
                changed = ui.changed() => {
                    if changed.is_err() { return; }
                    let next = self.profile_dependency_input();
                    if next == input { continue; }
                    true
                },
            };
            if demand {
                if sweep.upper.is_some() && !sweep.finished {
                    // Preserve fairness within the captured cut even while
                    // source/dependency arrivals keep this owner runnable.
                    sweep.resweep = true;
                } else {
                    sweep = ProfileRetrySweep::default();
                }
            }
            // Never acknowledge arrivals after the worker: a change concurrent
            // with its snapshot must retain one following turn.
            let _ = *source.borrow_and_update();
            let _ = *maintenance.borrow_and_update();
            let _ = *roster.borrow_and_update();
            let _ = *ui.borrow_and_update();
            let _ = *network.borrow_and_update();
            input = self.profile_dependency_input();
            #[cfg(test)]
            self.trace_account_client(&self.identity.public(), || {
                "profile_maintenance_turn".into()
            });
            let Some(work) = self.runtime_work.admit() else {
                return;
            };
            let node = self.clone();
            let result = work
                .spawn_blocking(move || {
                    let result = node.profile_maintenance_turn(&mut sweep);
                    (sweep, result)
                })
                .await
                .map(|value| value.into_inner());
            let outcome = match result {
                Ok((next, Ok(outcome))) => {
                    sweep = next;
                    outcome
                }
                Ok((next, Err(error))) => {
                    sweep = next;
                    log::warn!("profile log compaction failed; will retry: {error}");
                    ProfileTurn::Retry
                }
                Err(error) => {
                    sweep = ProfileRetrySweep::default();
                    log::warn!("profile compaction task failed: {error}");
                    ProfileTurn::Retry
                }
            };
            continuation = matches!(outcome, ProfileTurn::Continue);
            deadline = match outcome {
                ProfileTurn::Continue => tokio::time::Instant::now() + Duration::from_millis(25),
                ProfileTurn::Blocked(at) => at,
                ProfileTurn::Settled => tokio::time::Instant::now() + PROFILE_REPAIR_INTERVAL,
                ProfileTurn::Retry => tokio::time::Instant::now() + PROFILE_COMPACTION_INTERVAL,
            };
            #[cfg(test)]
            if continuation {
                self.trace_account_client(&self.identity.public(), || {
                    "profile_maintenance_continue".into()
                });
            }
            #[cfg(test)]
            if !continuation
                && !source.has_changed().unwrap_or(true)
                && !maintenance.has_changed().unwrap_or(true)
            {
                self.trace_account_client(&self.identity.public(), || {
                    "profile_maintenance_wait".into()
                });
            }
        }
    }
}

#[cfg(test)]
mod compaction_lease_tests {
    use super::*;
    use crate::identity::{account::Account, device::DeviceIdentity};

    fn owner_node(path: &std::path::Path) -> Arc<Node> {
        let proof = crate::discovery::Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "peer",
            9,
        );
        let (node, _, _) = super::super::test_support::node_with_files(
            path,
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        node
    }
    fn start_owner(
        node: &Arc<Node>,
    ) -> (
        tokio::task::JoinHandle<()>,
        tokio::sync::mpsc::UnboundedReceiver<bool>,
    ) {
        let (sent, received) = tokio::sync::mpsc::unbounded_channel();
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new(move |_, phase| {
            if phase == "profile_maintenance_wait" || phase == "profile_maintenance_continue" {
                let _ = sent.send(phase == "profile_maintenance_wait");
            }
        }));
        (
            tokio::spawn(node.clone().run_profile_compaction_loop()),
            received,
        )
    }
    async fn wait_idle(received: &mut tokio::sync::mpsc::UnboundedReceiver<bool>) -> bool {
        // Keep paused Tokio time fixed: an unobserved dependency wake must not
        // accidentally pass by advancing to the 300-second repair deadline.
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < until {
            if received.try_recv().is_ok_and(|settled| settled) {
                return true;
            }
            tokio::task::yield_now().await;
        }
        false
    }
    async fn stop_owner(node: &Arc<Node>, owner: tokio::task::JoinHandle<()>) {
        node.runtime_work.close();
        owner.abort();
        let _ = owner.await;
        node.runtime_work.drain().await;
    }
    async fn settle_owner(received: &mut tokio::sync::mpsc::UnboundedReceiver<bool>) -> bool {
        for _ in 0..128 {
            let until = std::time::Instant::now() + Duration::from_secs(5);
            let mut turn = None;
            while std::time::Instant::now() < until {
                if let Ok(settled) = received.try_recv() {
                    turn = Some(settled);
                    break;
                }
                tokio::task::yield_now().await;
            }
            match turn {
                Some(true) => return true,
                Some(false) => tokio::time::advance(Duration::from_millis(25)).await,
                None => return false,
            }
        }
        false
    }
    fn pending_profile(
        node: &Node,
        author: &DeviceIdentity,
        account: &Account,
        revision: u64,
    ) -> EventId {
        let profile = AccountProfile::new(account, revision, "profile".into(), "bio".into(), None);
        let event = Event::new(
            author,
            dm_conversation_id(&author.public(), &node.identity.public()),
            revision,
            vec![],
            revision,
            1,
            EventKind::Profile,
            crate::dm::seal(
                author,
                &node.identity.public().x25519_pub,
                &profile.encode(),
            )
            .unwrap(),
        );
        let id = event.id;
        node.log.lock().unwrap().append(event).unwrap();
        id
    }

    #[tokio::test(start_paused = true)]
    async fn profile_owner_finishes_finite_retry_sweep_past_sixteen_blocked_authors() {
        let directory = tempfile::tempdir().unwrap();
        let node = owner_node(directory.path());
        for _ in 0..16 {
            pending_profile(&node, &DeviceIdentity::generate(), &Account::generate(), 1);
        }
        let author = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = crate::discovery::Announce::new_with_account(&author, &account, "ready", 9);
        node.roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.1".parse().unwrap(), &node.user_id());
        let id = pending_profile(&node, &author, &account, 1);
        let (owner, mut idle) = start_owner(&node);
        // Observe each real completion before advancing its continuation clock.
        let mut finished = false;
        for _ in 0..32 {
            let until = std::time::Instant::now() + Duration::from_secs(5);
            let mut turn = None;
            while std::time::Instant::now() < until {
                if let Ok(settled) = idle.try_recv() {
                    turn = Some(settled);
                    break;
                }
                tokio::task::yield_now().await;
            }
            match turn {
                Some(true) => {
                    finished = true;
                    break;
                }
                Some(false) => tokio::time::advance(Duration::from_millis(25)).await,
                None => break,
            }
        }
        let processed = node
            .profile_receive_work
            .lock()
            .unwrap()
            .processed(id)
            .unwrap();
        stop_owner(&node, owner).await;
        assert!(
            finished && processed,
            "a finite fair sweep must reach the eligible row after16blocked candidates"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn profile_owner_hidden_proof_wake_retries_without_waiting_for_repair() {
        let directory = tempfile::tempdir().unwrap();
        let node = owner_node(directory.path());
        node.configure_privacy(
            directory.path(),
            "pw",
            &node.signed_announce("owner", 9),
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
        let author = DeviceIdentity::generate();
        let account = Account::generate();
        let id = pending_profile(&node, &author, &account, 1);
        let (owner, mut idle) = start_owner(&node);
        let initial = wait_idle(&mut idle).await;
        let proof = crate::discovery::Announce::new_with_account(&author, &account, "hidden", 9);
        let now = tokio::time::Instant::now();
        node.remember_peer_with_visibility(
            &author.public(),
            Some(&proof),
            "127.0.0.1".parse().unwrap(),
            false,
        )
        .unwrap();
        let recovered = wait_idle(&mut idle).await;
        let processed = node
            .profile_receive_work
            .lock()
            .unwrap()
            .processed(id)
            .unwrap();
        let clock = tokio::time::Instant::now();
        stop_owner(&node, owner).await;
        assert!(initial && recovered && processed);
        assert_eq!(
            clock, now,
            "a real dependency wake must not need a timer advance"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn profile_owner_discovers_profile_after_a_full_non_profile_header_page() {
        let directory = tempfile::tempdir().unwrap();
        let node = owner_node(directory.path());
        let author = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = crate::discovery::Announce::new_with_account(&author, &account, "ready", 9);
        node.roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.1".parse().unwrap(), &node.user_id());
        let conversation = dm_conversation_id(&author.public(), &node.identity.public());
        for seq in 1..=128 {
            node.log
                .lock()
                .unwrap()
                .append(Event::new(
                    &author,
                    conversation,
                    seq,
                    vec![],
                    seq,
                    0,
                    EventKind::Message,
                    vec![],
                ))
                .unwrap();
        }
        let id = pending_profile(&node, &author, &account, 129);
        let (owner, mut idle) = start_owner(&node);
        let mut finished = false;
        for _ in 0..32 {
            let until = std::time::Instant::now() + Duration::from_secs(5);
            let mut turn = None;
            while std::time::Instant::now() < until {
                if let Ok(settled) = idle.try_recv() {
                    turn = Some(settled);
                    break;
                }
                tokio::task::yield_now().await;
            }
            match turn {
                Some(true) => {
                    finished = true;
                    break;
                }
                Some(false) => tokio::time::advance(Duration::from_millis(25)).await,
                None => break,
            }
        }
        let processed = node
            .profile_receive_work
            .lock()
            .unwrap()
            .processed(id)
            .unwrap();
        stop_owner(&node, owner).await;
        assert!(
            finished && processed,
            "later discovery pages must retain immediate receive work after an initially empty retry sweep"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn profile_owner_final_lease_release_retains_immediate_work() {
        let directory = tempfile::tempdir().unwrap();
        let node = owner_node(directory.path());
        let lease = node.acquire_profile_compaction_lease().unwrap();
        let (owner, mut idle) = start_owner(&node);
        let initial = wait_idle(&mut idle).await;
        let now = tokio::time::Instant::now();
        drop(lease);
        let released = wait_idle(&mut idle).await;
        let clock = tokio::time::Instant::now();
        stop_owner(&node, owner).await;
        assert!(initial && released);
        assert_eq!(clock, now);
    }

    #[test]
    fn profile_dependency_wake_survives_late_route_failure_and_route_restoration() {
        let directory = tempfile::tempdir().unwrap();
        let node = owner_node(directory.path());
        node.configure_privacy(
            directory.path(),
            "pw",
            &node.signed_announce("owner", 9),
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
        let author = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = crate::discovery::Announce::new_with_account(&author, &account, "hidden", 9);
        let route_path = directory.path().join("peer-routes");
        if route_path.is_file() {
            std::fs::remove_file(&route_path).unwrap();
        }
        std::fs::create_dir(&route_path).unwrap();
        let mut changes = node.storage.database().subscribe_maintenance_changes();
        assert!(
            node.remember_peer_with_visibility(
                &author.public(),
                Some(&proof),
                "127.0.0.1".parse().unwrap(),
                false
            )
            .is_err()
        );
        assert!(
            changes.has_changed().unwrap(),
            "a committed proof retains its wake despite later route persistence failure"
        );
        assert!(node.privacy.state.try_write().is_ok());
        let _ = *changes.borrow_and_update();
        let revision = node
            .privacy
            .state
            .read()
            .unwrap()
            .as_ref()
            .unwrap()
            .proofs
            .revision();
        std::fs::remove_dir(&route_path).unwrap();
        node.remember_peer_with_visibility(
            &author.public(),
            Some(&proof),
            "127.0.0.1".parse().unwrap(),
            false,
        )
        .unwrap();
        assert_eq!(
            node.privacy
                .state
                .read()
                .unwrap()
                .as_ref()
                .unwrap()
                .proofs
                .revision(),
            revision
        );
        assert!(
            changes.has_changed().unwrap(),
            "restoring the route is a dependency change even with the same proof"
        );
        let _ = *changes.borrow_and_update();
        node.remember_peer_with_visibility(
            &author.public(),
            Some(&proof),
            "127.0.0.1".parse().unwrap(),
            false,
        )
        .unwrap();
        assert!(
            !changes.has_changed().unwrap(),
            "unchanged hidden admission cannot manufacture maintenance demand"
        );
    }

    #[test]
    fn discovered_snapshot_wakes_only_committed_proof_or_route_fact_changes() {
        let directory = tempfile::tempdir().unwrap();
        let node = owner_node(directory.path());
        node.configure_privacy(
            directory.path(),
            "pw",
            &node.signed_announce("owner", 9),
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
        let author = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = crate::discovery::Announce::new_with_account(&author, &account, "known", 9);
        node.roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.1".parse().unwrap(), &node.user_id());
        let mut changes = node.storage.database().subscribe_maintenance_changes();
        node.cached_peer_snapshot().unwrap();
        assert!(changes.has_changed().unwrap());
        let _ = *changes.borrow_and_update();
        node.cached_peer_snapshot().unwrap();
        assert!(!changes.has_changed().unwrap());
        node.roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.2".parse().unwrap(), &node.user_id());
        node.cached_peer_snapshot().unwrap();
        assert!(changes.has_changed().unwrap());
        let _ = *changes.borrow_and_update();
        node.roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.1".parse().unwrap(), &node.user_id());
        node.cached_peer_snapshot().unwrap();
        assert!(
            !changes.has_changed().unwrap(),
            "known route preference/freshness must remain quiet"
        );
        let renamed = crate::discovery::Announce::new_with_account(&author, &account, "renamed", 9);
        node.roster
            .lock()
            .unwrap()
            .update(&renamed, "127.0.0.1".parse().unwrap(), &node.user_id());
        node.cached_peer_snapshot().unwrap();
        assert!(changes.has_changed().unwrap());
        assert!(node.privacy.state.try_write().is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn profile_owner_real_policy_change_retries_more_than_sixteen_blocked_profiles() {
        let directory = tempfile::tempdir().unwrap();
        let node = owner_node(directory.path());
        node.set_device_sync_category(super::super::SyncCategory::Profile, false)
            .unwrap();
        let author = DeviceIdentity::generate();
        let proof =
            crate::discovery::Announce::new_with_account(&author, &node.account, "sibling", 9);
        node.roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.1".parse().unwrap(), &node.user_id());
        let ids: Vec<_> = (1..=17)
            .map(|revision| pending_profile(&node, &author, &node.account, revision))
            .collect();
        let (owner, mut idle) = start_owner(&node);
        let initial = settle_owner(&mut idle).await;
        let mut changes = node.storage.database().subscribe_maintenance_changes();
        node.set_device_sync_category(super::super::SyncCategory::Profile, false)
            .unwrap();
        let quiet = !changes.has_changed().unwrap();
        let _ = *changes.borrow_and_update();
        node.set_device_sync_category(super::super::SyncCategory::Profile, true)
            .unwrap();
        let changed = changes.has_changed().unwrap();
        let recovered = settle_owner(&mut idle).await;
        let all_processed = ids.iter().all(|id| {
            node.profile_receive_work
                .lock()
                .unwrap()
                .processed(*id)
                .unwrap()
        });
        stop_owner(&node, owner).await;
        assert!(initial && quiet && changed && recovered && all_processed);
    }

    #[tokio::test(start_paused = true)]
    async fn profile_owner_storage_failure_keeps_the_three_second_retry() {
        let directory = tempfile::tempdir().unwrap();
        let node = owner_node(directory.path());
        let author = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = crate::discovery::Announce::new_with_account(&author, &account, "ready", 9);
        node.roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.1".parse().unwrap(), &node.user_id());
        let id = pending_profile(&node, &author, &account, 1);
        node.storage.database().transaction(|c| {
            c.execute_batch("CREATE TEMP TRIGGER fail_profile_owner_scan BEFORE UPDATE ON profile_receive_scan BEGIN SELECT RAISE(ABORT,'profile owner fault'); END")?;Ok(())
        }).unwrap();
        let (owner, mut idle) = start_owner(&node);
        let failed = wait_idle(&mut idle).await;
        let before = node
            .profile_receive_work
            .lock()
            .unwrap()
            .processed(id)
            .unwrap();
        node.storage
            .database()
            .transaction(|c| {
                c.execute_batch("DROP TRIGGER temp.fail_profile_owner_scan")?;
                Ok(())
            })
            .unwrap();
        tokio::time::advance(Duration::from_secs(3)).await;
        let recovered = settle_owner(&mut idle).await;
        let processed = node
            .profile_receive_work
            .lock()
            .unwrap()
            .processed(id)
            .unwrap();
        stop_owner(&node, owner).await;
        assert!(failed && !before && recovered && processed);
    }

    #[tokio::test(start_paused = true)]
    async fn profile_owner_dependency_wake_flood_preserves_the_captured_retry_cursor() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let directory = tempfile::tempdir().unwrap();
        let node = owner_node(directory.path());
        for _ in 0..16 {
            pending_profile(&node, &DeviceIdentity::generate(), &Account::generate(), 1);
        }
        let author = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = crate::discovery::Announce::new_with_account(&author, &account, "ready", 9);
        node.roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.1".parse().unwrap(), &node.user_id());
        let target = pending_profile(&node, &author, &account, 1);
        let weak = Arc::downgrade(&node);
        let waves = Arc::new(AtomicUsize::new(0));
        let observed = waves.clone();
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let completion = completed.clone();
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new(move |_, phase| {
            if phase == "profile_maintenance_continue" {
                if let Some(node) = weak.upgrade() {
                    completion.store(
                        node.profile_receive_work
                            .lock()
                            .unwrap()
                            .processed(target)
                            .unwrap(),
                        Ordering::SeqCst,
                    );
                }
                let wave = observed.fetch_add(1, Ordering::SeqCst);
                if wave < 64
                    && let Some(node) = weak.upgrade()
                {
                    let event = Event::new(
                        &node.identity,
                        ConversationId::new([76; 32]),
                        wave as u64 + 1,
                        vec![],
                        wave as u64 + 1,
                        0,
                        EventKind::Message,
                        vec![],
                    );
                    node.log.lock().unwrap().append(event).unwrap();
                }
            }
        }));
        let owner = tokio::spawn(node.clone().run_profile_compaction_loop());
        let until = std::time::Instant::now() + Duration::from_secs(5);
        let mut processed = false;
        while std::time::Instant::now() < until {
            if completed.load(Ordering::SeqCst) {
                processed = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        let wave_count = waves.load(Ordering::SeqCst);
        stop_owner(&node, owner).await;
        assert!(
            processed,
            "real unrelated source wakes reset the active cut before its eligible17th candidate; waves={wave_count}"
        );
        assert!(
            wave_count > 0,
            "the fixture must produce an actual continuation/source wake"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn profile_owner_dependency_cache_inputs_persist_without_heartbeat_work() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let directory = tempfile::tempdir().unwrap();
        let node = owner_node(directory.path());
        node.configure_privacy(
            directory.path(),
            "pw",
            &node.signed_announce("owner", 9),
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
        let author = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = crate::discovery::Announce::new_with_account(&author, &account, "known", 9);
        for ip in ["127.0.0.1", "127.0.0.2"] {
            node.roster
                .lock()
                .unwrap()
                .update(&proof, ip.parse().unwrap(), &node.user_id());
        }
        let turns = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(AtomicUsize::new(0));
        let t = turns.clone();
        let s = seen.clone();
        let (sent, mut idle) = tokio::sync::mpsc::unbounded_channel();
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new(move |_, phase| {
            if phase == "profile_maintenance_turn" {
                t.fetch_add(1, Ordering::SeqCst);
            }
            if phase == "profile_maintenance_roster_observed" {
                s.fetch_add(1, Ordering::SeqCst);
            }
            if phase == "profile_maintenance_wait" || phase == "profile_maintenance_continue" {
                let _ = sent.send(phase == "profile_maintenance_wait");
            }
        }));
        let owner = tokio::spawn(node.clone().run_profile_compaction_loop());
        let initial = settle_owner(&mut idle).await;
        let before = turns.load(Ordering::SeqCst);
        let mut observed = true;
        for ip in ["127.0.0.1", "127.0.0.2"] {
            let prior = seen.load(Ordering::SeqCst);
            node.roster
                .lock()
                .unwrap()
                .update(&proof, ip.parse().unwrap(), &node.user_id());
            let until = std::time::Instant::now() + Duration::from_secs(5);
            while seen.load(Ordering::SeqCst) == prior && std::time::Instant::now() < until {
                tokio::task::yield_now().await;
            }
            observed &= seen.load(Ordering::SeqCst) > prior;
        }
        let quiet = turns.load(Ordering::SeqCst) == before;
        let renamed =
            crate::discovery::Announce::new_with_account(&author, &account, "renamed", 10);
        node.roster
            .lock()
            .unwrap()
            .update(&renamed, "127.0.0.2".parse().unwrap(), &node.user_id());
        let renamed_done = wait_idle(&mut idle).await;
        let stored_name = node
            .privacy
            .state
            .read()
            .unwrap()
            .as_ref()
            .unwrap()
            .proofs
            .by_author(&author.public().ed25519_pub)
            .is_some_and(|proof| proof.name == "renamed" && proof.tcp_port == 10);
        node.roster
            .lock()
            .unwrap()
            .update(&renamed, "127.0.0.3".parse().unwrap(), &node.user_id());
        let expanded_done = wait_idle(&mut idle).await;
        let stored_route = node
            .privacy
            .state
            .read()
            .unwrap()
            .as_ref()
            .unwrap()
            .routes
            .contains(&author.public(), "127.0.0.3".parse().unwrap());
        stop_owner(&node, owner).await;
        assert!(
            initial
                && observed
                && quiet
                && renamed_done
                && stored_name
                && expanded_done
                && stored_route,
            "cache facts need actual work, heartbeat preference must stay quiet: initial={initial} observed={observed} quiet={quiet} renamed_done={renamed_done} stored_name={stored_name} expanded_done={expanded_done} stored_route={stored_route}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn settled_profile_owner_does_not_admit_another_three_second_pass() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let directory = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let proof =
            crate::discovery::Announce::new_with_account(&peer, &Account::generate(), "peer", 9);
        let (node, _, _) = super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let turns = Arc::new(AtomicUsize::new(0));
        let observed = turns.clone();
        let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new(move |_, phase| {
            if phase == "profile_maintenance_turn" {
                observed.fetch_add(1, Ordering::SeqCst);
            }
            if phase == "profile_maintenance_wait" {
                let _ = sent.send(());
            }
        }));
        let owner = tokio::spawn(node.clone().run_profile_compaction_loop());
        received
            .recv()
            .await
            .expect("initial real maintenance pass");
        // Account for the bounded initial discovery/maintenance slice and its
        // retained followup before observing a settled three-second interval.
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
        let before = turns.load(Ordering::SeqCst);
        tokio::time::advance(Duration::from_secs(3)).await;
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
        let after = turns.load(Ordering::SeqCst);
        node.runtime_work.close();
        owner.abort();
        let _ = owner.await;
        node.runtime_work.drain().await;
        assert_eq!(
            after, before,
            "a settled owner must wait for demand or sparse repair"
        );
    }

    #[test]
    fn global_profile_recovery_retries_unknown_work_after_verified_proof_arrives() {
        let directory = tempfile::tempdir().unwrap();
        let known = DeviceIdentity::generate();
        let known_account = Account::generate();
        let proof =
            crate::discovery::Announce::new_with_account(&known, &known_account, "Known", 9);
        let (node, _, _) = super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let source = DeviceIdentity::generate();
        let account = Account::generate();
        let conversation = dm_conversation_id(&source.public(), &node.identity.public());
        let profile = AccountProfile::new(&account, 1, "Recovered".into(), "bio".into(), None);
        let event = Event::new(
            &source,
            conversation,
            1,
            vec![],
            1,
            1,
            EventKind::Profile,
            crate::dm::seal(
                &source,
                &node.identity.public().x25519_pub,
                &profile.encode(),
            )
            .unwrap(),
        );
        let id = event.id;
        node.log.lock().unwrap().append(event).unwrap();
        node.drain_profile_receptions(None).unwrap();
        assert!(
            !node
                .profile_receive_work
                .lock()
                .unwrap()
                .processed(id)
                .unwrap()
        );
        let reads = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        assert_eq!(reads, (0, 0));
        let proof = crate::discovery::Announce::new_with_account(&source, &account, "Recovered", 9);
        node.roster.lock().unwrap().update(
            &proof,
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            &node.user_id(),
        );
        node.drain_profile_receptions(None).unwrap();
        assert!(
            node.profile_receive_work
                .lock()
                .unwrap()
                .processed(id)
                .unwrap()
        );
        assert_eq!(
            node.account_profile(&account.account_id()).unwrap(),
            Some(profile)
        );
    }

    #[tokio::test]
    async fn ordinary_sync_rehydrates_compacted_profiles_once_then_reaches_a_fixed_point() {
        use crate::eventlog::{AppendOutcome, sync::network::NetworkSyncStore};

        async fn reconcile(source: &Arc<Node>, receiver: &Arc<Node>, conversation: ConversationId) {
            let (client, server) = tokio::io::duplex(256 * 1024);
            let source_node = source.clone();
            let task = tokio::spawn(async move {
                let mut channel =
                    crate::transport::SecureChannel::accept(server, &source_node.identity)
                        .await
                        .unwrap();
                let store = source_node.serve_sync_store(channel.peer_identity());
                loop {
                    if matches!(
                        super::super::session::serve_one_paged(&mut channel, &store)
                            .await
                            .unwrap(),
                        super::super::session::Served::Closed
                    ) {
                        break;
                    }
                }
            });
            let mut channel = crate::transport::SecureChannel::connect(
                client,
                &receiver.identity,
                Some(&source.identity.public()),
            )
            .await
            .unwrap();
            let store = receiver.sync_store(channel.peer_identity());
            super::super::session::request_round_paged(&mut channel, &store, conversation)
                .await
                .unwrap();
            drop(store);
            drop(channel);
            task.await.unwrap();
        }
        fn ordinals(node: &Node, ids: &[EventId]) -> Vec<Option<i64>> {
            let rows = node
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .committed_headers_page(None, 128)
                .unwrap();
            ids.iter()
                .map(|id| {
                    rows.iter()
                        .find(|row| row.header.id == *id)
                        .map(|row| row.ordinal)
                })
                .collect()
        }
        fn writes(node: &Node) -> (usize, u64) {
            let database = node.storage.database();
            (
                database.successful_write_commits(),
                database
                    .read(|connection| Ok(connection.total_changes()))
                    .unwrap(),
            )
        }
        fn drain(node: &Node, conversation: ConversationId) -> usize {
            // The normal receive owner alone discovers/requests compaction.
            // Bound local turns without issuing any artificial maintenance demand.
            let mut dropped = 0;
            for _ in 0..64 {
                node.process_profile_events(conversation);
                dropped += node.drain_profile_compaction().unwrap().unwrap_or(0);
            }
            dropped
        }

        let directory = tempfile::tempdir().unwrap();
        let author = DeviceIdentity::generate();
        let author_account = Account::generate();
        let recipient = DeviceIdentity::generate();
        let recipient_account = Account::generate();
        let source_proof =
            crate::discovery::Announce::new_with_account(&author, &author_account, "Source", 9);
        let receiver_proof = crate::discovery::Announce::new_with_account(
            &recipient,
            &recipient_account,
            "Receiver",
            10,
        );
        let source_path = directory.path().join("source");
        let receiver_path = directory.path().join("receiver");
        std::fs::create_dir_all(&source_path).unwrap();
        std::fs::create_dir_all(&receiver_path).unwrap();
        let (source, _, _) = super::super::test_support::node_with_files(
            &source_path,
            author,
            author_account,
            &receiver_proof,
        );
        let (receiver, _, _) = super::super::test_support::node_with_files(
            &receiver_path,
            recipient,
            recipient_account,
            &source_proof,
        );
        let conversation =
            dm_conversation_id(&source.identity.public(), &receiver.identity.public());
        let mut profiles = Vec::new();
        // Production compaction requires eight droppable leaves and retains the
        // latest pair. Ten tiny real Profiles meet that gate without a large DB.
        for revision in 1..=10 {
            let payload =
                ProfilePayload::new(&source.account, Some(vec![revision as u8]), revision);
            let event = Event::new(
                &source.identity,
                conversation,
                revision,
                vec![],
                revision,
                1,
                EventKind::Profile,
                crate::dm::seal(
                    &source.identity,
                    &receiver.identity.public().x25519_pub,
                    &payload.encode(),
                )
                .unwrap(),
            );
            source.log.lock().unwrap().append(event.clone()).unwrap();
            profiles.push(event);
        }
        let ids: Vec<_> = profiles.iter().map(|event| event.id).collect();
        reconcile(&source, &receiver, conversation).await;
        let original_ordinals = ordinals(&receiver, &ids);
        assert!(original_ordinals.iter().all(Option::is_some));
        receiver.process_profile_events(conversation);
        assert_eq!(
            drain(&receiver, conversation),
            8,
            "natural receive discovery must drive actual production compaction"
        );
        assert_eq!(
            receiver
                .log
                .lock()
                .unwrap()
                .count_kind(&conversation, EventKind::Profile)
                .unwrap(),
            2
        );
        let compacted = ordinals(&receiver, &ids);
        assert!(compacted[..8].iter().all(Option::is_none));
        assert!(compacted[8..].iter().all(Option::is_some));
        for event in &profiles[..3] {
            assert!(
                receiver
                    .log
                    .lock()
                    .unwrap()
                    .get(&event.id)
                    .unwrap()
                    .is_none()
            );
            assert!(
                receiver
                    .profile_receive_work
                    .lock()
                    .unwrap()
                    .processed(event.id)
                    .unwrap(),
                "exact processed evidence must predate ordinary reimport"
            );
        }
        let before = receiver
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .source_revision()
            .unwrap();
        {
            let owner = receiver.sync_store(&source.identity.public());
            let mut store = owner.lock().unwrap();
            for event in &profiles[..3] {
                assert_eq!(
                    NetworkSyncStore::ingest(&mut *store, event.clone()).unwrap(),
                    AppendOutcome::Appended
                );
            }
        }
        let rehydrated = ordinals(&receiver, &ids);
        for index in 0..3 {
            assert!(
                rehydrated[index] > original_ordinals[index],
                "same immutable ID must have a newly committed ordinal"
            );
        }
        assert!(
            receiver
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .source_revision()
                .unwrap()
                > before
        );
        receiver.process_profile_events(conversation);
        assert_eq!(
            drain(&receiver, conversation),
            0,
            "processed reimports alone must not manufacture new compaction demand"
        );
        assert_eq!(
            receiver
                .log
                .lock()
                .unwrap()
                .count_kind(&conversation, EventKind::Profile)
                .unwrap(),
            5
        );
        assert_eq!(ordinals(&receiver, &ids), rehydrated);

        let mut settled = None;
        for round in 0..3 {
            let before_writes = (writes(&source), writes(&receiver));
            reconcile(&source, &receiver, conversation).await;
            let after_sync_writes = (writes(&source), writes(&receiver));
            receiver.process_profile_events(conversation);
            let dropped = drain(&receiver, conversation);
            let after_writes = (writes(&source), writes(&receiver));
            let current = ordinals(&receiver, &ids);
            let revision = receiver
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .source_revision()
                .unwrap();
            eprintln!(
                "profile rehydration round={round} retained={} dropped={dropped} source_revision={revision} ordinals={current:?} writes_before={before_writes:?} writes_after_sync={after_sync_writes:?} writes_after_maintenance={after_writes:?}",
                receiver
                    .log
                    .lock()
                    .unwrap()
                    .count_kind(&conversation, EventKind::Profile)
                    .unwrap()
            );
            assert_eq!(
                dropped, 0,
                "unchanged ordinary reconciliation must not prune and rehydrate the same IDs repeatedly"
            );
            assert!(current.iter().all(Option::is_some));
            if let Some((previous, previous_revision)) = &settled {
                assert_eq!(
                    &current, previous,
                    "unchanged round must retain exact raw commit identities"
                );
                assert_eq!(
                    revision, *previous_revision,
                    "unchanged round must reach a source fixed point"
                );
                assert_eq!(
                    after_writes, before_writes,
                    "unchanged sync plus normal maintenance must also reach a SQL write fixed point"
                );
            }
            settled = Some((current, revision));
        }
        assert_eq!(
            receiver.peer_avatar(&source.account_id()).unwrap(),
            Some(vec![10])
        );
    }

    #[test]
    fn nested_repair_leases_defer_profile_compaction_until_final_release() {
        let directory = tempfile::tempdir().unwrap();
        let remote = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = crate::discovery::Announce::new_with_account(&remote, &account, "Peer", 9);
        let (node, _, _) = super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let conversation = dm_conversation_id(&node.identity.public(), &remote.public());
        for revision in 1..=12 {
            let payload = ProfilePayload::new(&account, None, revision);
            let event = Event::new(
                &remote,
                conversation,
                revision,
                vec![],
                revision,
                0,
                EventKind::Profile,
                crate::dm::seal(
                    &remote,
                    &node.identity.public().x25519_pub,
                    &payload.encode(),
                )
                .unwrap(),
            );
            node.log.lock().unwrap().append(event).unwrap();
        }
        node.log
            .lock()
            .unwrap()
            .request_profile_compaction(conversation)
            .unwrap();
        let first = node.acquire_profile_compaction_lease().unwrap();
        let second = node.acquire_profile_compaction_lease().unwrap();
        for _ in 0..4 {
            assert_eq!(
                node.drain_profile_compaction().unwrap(),
                None,
                "dependency recovery must exclude physical Profile compaction"
            );
        }
        drop(first);
        assert_eq!(node.drain_profile_compaction().unwrap(), None);
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .count_kind(&conversation, EventKind::Profile)
                .unwrap(),
            12
        );
        drop(second);
        let mut dropped = 0;
        for _ in 0..64 {
            dropped += node.drain_profile_compaction().unwrap().unwrap_or(0);
            if dropped == 10 {
                break;
            }
        }
        assert_eq!(
            dropped, 10,
            "last lease release must allow queued compaction"
        );
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .count_kind(&conversation, EventKind::Profile)
                .unwrap(),
            2
        );
    }
}
