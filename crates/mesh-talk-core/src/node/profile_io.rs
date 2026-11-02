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
        if let Err(error) = self.drain_profile_receptions().and_then(|_| {
            self.log
                .lock()
                .expect("log mutex not poisoned")
                .request_profile_compaction(conv)?;
            Ok(())
        }) {
            log::warn!("profile reception deferred after storage failure: {error}");
        }
    }

    fn drain_profile_receptions(&self) -> Result<bool, NodeError> {
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
        work.discover(
            after,
            &headers,
            Author::from_ed25519(self.identity.public().ed25519_pub),
        )?;
        for candidate in work.candidates()? {
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
        self.storage.maintain_participant_events()?;
        self.log
            .lock()
            .expect("log mutex not poisoned")
            .repository()?
            .cleanup_selections()?;
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
                    let discovering = node.drain_profile_receptions()?;
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
