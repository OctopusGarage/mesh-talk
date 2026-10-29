//! Publishing our own avatar as a signed profile, and receiving peers' profiles. The
//! profile rides the SAME reliable per-device-pair sync as account DMs: it's sealed to
//! each peer device, appended as an `EventKind::Profile` event, delivered direct, and
//! replicated to the post office — so it converges like a message, not via a fragile
//! one-shot push. Split out of `node.rs` (one `impl Node` block per domain).

use super::*;
use crate::discovery::roster::PeerRecord;
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
            if let Some(mut profile) = store.get(&self.account_id()) {
                if profile.avatar == avatar {
                    // Repair a previous committed intent before acknowledging a retry.
                    self.account_profiles
                        .receive_account_profile(&profile)
                        .map_err(NodeError::Log)?;
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
                self.account_profiles
                    .receive_account_profile(&profile)
                    .map_err(NodeError::Log)?;
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
    pub(in crate::node) fn public_profile_revision(&self) -> Option<u64> {
        let card = self
            .profiles
            .lock()
            .expect("profiles mutex not poisoned")
            .get(&self.account_id())
            .map(|card| card.revision);
        let avatar = self
            .account_profiles
            .current()
            .map(|profile| profile.updated_at);
        card.into_iter().chain(avatar).max()
    }

    /// Report whether direct reconciliation or relay publication completed, so
    /// a transient failure remains eligible for a later runtime poll.
    pub(in crate::node) async fn deliver_profile_to(
        &self,
        peer: &PeerRecord,
        conv: ConversationId,
    ) -> bool {
        let direct = async {
            self.adaptive_control_timeout(&peer.public, self.deliver_direct(peer, conv))
                .await
                .is_some_and(|result| result.is_ok())
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

    /// Decode + verify + store any new `Profile` events in `conv` authored by others, then
    /// surface each applied avatar update to the app. A profile is marked emitted only
    /// after it's processed (verified-and-applied, or definitively rejected), so a
    /// transiently-unopenable one (author not yet in the roster) is retried on a later
    /// sync rather than lost — mirroring `emit_new_messages`.
    pub(in crate::node) fn process_profile_events(&self, conv: ConversationId) {
        let result = (|| -> Result<(), NodeError> {
            let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
            let candidates: Vec<Event> = {
                let log = self.log.lock().expect("log mutex not poisoned");
                let emitted = self
                    .profiles_emitted
                    .lock()
                    .expect("profiles_emitted mutex not poisoned");
                log.events(&conv)?
                    .into_iter()
                    .filter(|e| {
                        e.kind == EventKind::Profile
                            && e.author != self_author
                            && !emitted.contains(&e.id)
                    })
                    .collect()
            };
            for event in candidates {
                let author_uid = event.author.user_id();
                // The author's X25519 (to open the sealed box) + their CERTIFIED account (to
                // bind the profile's claimed account — a device can't forge another account's
                // avatar). Unknown author → retry later (NOT marked emitted).
                let (sender_x25519, peer_account) = {
                    match self.routing_peer(&author_uid) {
                        Some(p) => (p.public.x25519_pub, p.account_id.clone()),
                        None => continue,
                    }
                };
                let plaintext =
                    match crate::dm::open(&self.identity, &sender_x25519, &event.ciphertext) {
                        Ok(p) => p,
                        Err(_) => continue, // not yet openable / not a profile we can read — retry later
                    };
                // From here the event is readable; mark it emitted regardless of outcome so a
                // malformed/forged/stale profile isn't reprocessed every sync.
                self.profiles_emitted
                    .lock()
                    .expect("profiles_emitted mutex not poisoned")
                    .insert(event.id);
                if plaintext.starts_with(b"MTP3") {
                    let Some(profile) = AccountProfile::decode(&plaintext) else {
                        continue;
                    };
                    if peer_account.as_deref() != Some(profile.account_id().as_str()) {
                        continue;
                    }
                    if profile.account_id() == self.account_id()
                        && !self.device_sync_policy().profile
                    {
                        self.profiles_emitted
                            .lock()
                            .expect("profiles_emitted mutex not poisoned")
                            .remove(&event.id);
                        continue;
                    }
                    if self.receive_account_profile(&profile).is_err() {
                        self.profiles_emitted
                            .lock()
                            .expect("profiles_emitted mutex not poisoned")
                            .remove(&event.id);
                    }
                    continue;
                }
                if plaintext.starts_with(b"MTC1") {
                    let Some(card) = TrustCard::decode(&plaintext) else {
                        continue;
                    };
                    if peer_account.as_deref() != Some(card.account_id().as_str()) {
                        continue;
                    }
                    if card.account_id() == self.account_id() && !self.device_sync_policy().profile
                    {
                        self.profiles_emitted
                            .lock()
                            .expect("profiles_emitted mutex not poisoned")
                            .remove(&event.id);
                        continue;
                    }
                    if self.receive_trust_card(&card).is_err() {
                        self.profiles_emitted
                            .lock()
                            .expect("profiles_emitted mutex not poisoned")
                            .remove(&event.id);
                    }
                    continue;
                }
                let Some(payload) = ProfilePayload::decode(&plaintext) else {
                    continue;
                };
                // Authenticated: the account signature must verify (also enforces the size
                // bound) AND the claimed account must match the AUTHENTICATED device's
                // certified account (re-homing/spoof defense).
                if !payload.verify()
                    || peer_account.as_deref() != Some(payload.account_id().as_str())
                {
                    continue;
                }
                if payload.account_id() == self.account_id() && !self.device_sync_policy().profile {
                    self.profiles_emitted
                        .lock()
                        .expect("profiles_emitted mutex not poisoned")
                        .remove(&event.id);
                    continue;
                }
                let applied = match self.receive_legacy_profile(&payload) {
                    Ok(applied) => applied,
                    Err(_) => {
                        self.profiles_emitted
                            .lock()
                            .expect("profiles_emitted mutex not poisoned")
                            .remove(&event.id);
                        continue;
                    }
                };
                if applied {
                    let _ = self.profile_incoming.send(ReceivedProfile {
                        account_id: payload.account_id(),
                        avatar: payload.avatar,
                    });
                }
            }
            // Bound on-disk growth: schedule dropping superseded, unreferenced profile events
            // for this conversation, preserving parents of retained legacy/card pairs. The accept
            // loop performs the expensive rewrite outside this sync callback.
            self.log
                .lock()
                .expect("log mutex not poisoned")
                .request_profile_compaction(conv)?;

            Ok(())
        })();
        if let Err(error) = result {
            log::warn!("process_profile_events deferred after storage failure: {error}");
        }
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
        loop {
            tokio::time::sleep(PROFILE_COMPACTION_INTERVAL).await;
            let node = Arc::clone(&self);
            let Some(work) = self.runtime_work.admit() else {
                return;
            };
            match work
                .spawn_blocking(move || {
                    if node.cache_discovered_peers().is_err() {
                        log::warn!("verified discovery cache update failed; will retry");
                    }
                    node.drain_profile_compaction()
                })
                .await
                .map(|value| value.into_inner())
            {
                Ok(Ok(_)) => {}
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
