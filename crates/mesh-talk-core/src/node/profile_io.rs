//! Publishing our own avatar as a signed profile, and receiving peers' profiles. The
//! profile rides the SAME reliable per-device-pair sync as account DMs: it's sealed to
//! each peer device, appended as an `EventKind::Profile` event, delivered direct, and
//! replicated to the post office — so it converges like a message, not via a fragile
//! one-shot push. Split out of `node.rs` (one `impl Node` block per domain).

use super::*;
use crate::discovery::roster::PeerRecord;
use crate::eventlog::LogError;
use crate::eventlog::event::{Author, ConversationId, Event, EventKind};
use crate::node::conversation::dm_conversation_id;
use crate::node::profile::ProfilePayload;
use std::sync::Arc;
use std::time::Duration;

/// Each pass reclaims at most one conversation; the persistent log also applies a cooldown.
const PROFILE_COMPACTION_INTERVAL: Duration = Duration::from_secs(3);

impl Node {
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
        let sealed = crate::dm::seal(&self.identity, &peer.public.x25519_pub, wire).ok()?;
        let (_, id) = self
            .append_event_with_profile_parent(conv, EventKind::Profile, sealed, required_parent)
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
        let work = match self.profile_receive_work.try_lock() {
            Ok(work) => work,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(false),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(NodeError::Log(crate::eventlog::LogError::CorruptFile(
                    "profile receive owner unavailable".into(),
                )));
            }
        };
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
        for candidate in work.candidates_for(conversation)? {
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
                if header.conversation != candidate.conversation
                    || header.kind != EventKind::Profile
                {
                    return Err(NodeError::Log(LogError::CorruptFile(
                        "profile retry source changed".into(),
                    )));
                }
                let Some(peer) = self.routing_peer(&header.author.user_id()) else {
                    continue;
                };
                if peer.account_id.as_deref() == Some(self.account_id().as_str())
                    && !self.device_sync_policy().profile
                {
                    continue;
                }
            } else {
                work.complete(candidate.id)?;
                self.note_reception_change();
                continue;
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
        }
        Ok(discovering)
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
        self.log
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
            return Ok(None);
        }
        self.log
            .lock()
            .expect("log mutex not poisoned")
            .drain_one_profile_compaction()
    }

    /// Owned by the accept-loop future, rather than a detached background task.
    pub(in crate::node) async fn run_profile_compaction_loop(self: Arc<Self>) {
        let mut delay = PROFILE_COMPACTION_INTERVAL;
        loop {
            tokio::time::sleep(delay).await;
            let node = Arc::clone(&self);
            let Some(work) = self.runtime_work.admit() else {
                return;
            };
            match work
                .spawn_blocking(move || {
                    if node.cache_discovered_peers().is_err() {
                        log::warn!("verified discovery cache update failed; will retry");
                    }
                    node.observe_order_work();
                    let discovering = node.drain_profile_receptions(None)?;
                    node.drain_profile_compaction()?;
                    Ok::<_, NodeError>(discovering)
                })
                .await
                .map(|value| value.into_inner())
            {
                Ok(Ok(discovering)) => {
                    delay = if discovering {
                        Duration::from_millis(25)
                    } else {
                        PROFILE_COMPACTION_INTERVAL
                    };
                }
                Ok(Err(error)) => {
                    log::warn!("profile log compaction failed; will retry: {error}");
                }
                Err(error) => {
                    log::warn!("profile compaction task failed: {error}");
                }
            }
        }
    }
}

#[cfg(test)]
mod compaction_lease_tests {
    use super::*;
    use crate::identity::{account::Account, device::DeviceIdentity};

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
