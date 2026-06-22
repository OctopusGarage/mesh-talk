//! Publishing our own avatar as a signed profile, and receiving peers' profiles. The
//! profile rides the SAME reliable per-device-pair sync as account DMs: it's sealed to
//! each peer device, appended as an `EventKind::Profile` event, delivered direct, and
//! replicated to the post office — so it converges like a message, not via a fragile
//! one-shot push. Split out of `node.rs` (one `impl Node` block per domain).

use super::node::now_millis;
use super::*;
use crate::discovery::roster::PeerRecord;
use crate::eventlog::event::{Author, ConversationId, Event, EventKind};
use crate::node::conversation::dm_conversation_id;
use crate::node::profile::{ProfilePayload, MAX_AVATAR_BYTES};

impl Node {
    /// Set (or clear) this user's OWN avatar and PROPAGATE it to peers. `avatar` is the
    /// small data-URL bytes the UI produces (or `None` to clear). A SET larger than
    /// [`MAX_AVATAR_BYTES`] is rejected. Builds a signed [`ProfilePayload`] (authenticated
    /// by the account key), records it as our current profile (so a freshly-discovered
    /// peer gets re-published the same version), then seals + appends + delivers one copy
    /// to every known peer device + our own other devices (the account fan-out set), so
    /// contacts actually see the photo. Best-effort delivery per device (the event is
    /// durably logged and syncs later if a peer is offline).
    pub async fn set_avatar(&self, avatar: Option<Vec<u8>>) -> Result<(), NodeError> {
        if let Some(a) = &avatar {
            if a.len() > MAX_AVATAR_BYTES {
                return Err(NodeError::Channel(format!(
                    "avatar too large: {} bytes (max {MAX_AVATAR_BYTES})",
                    a.len()
                )));
            }
        }
        let payload = ProfilePayload::new(&self.account, avatar, now_millis());
        // Hold it so on-discovery re-publish sends the current version.
        *self
            .my_profile
            .lock()
            .expect("my_profile mutex not poisoned") = Some(payload.clone());
        self.publish_profile_to_peers(&payload).await;
        Ok(())
    }

    /// Seal + append + deliver `payload` to every peer device we'd fan an account message
    /// out to (every known device of every known account, plus our own other devices),
    /// deduped by user-id. Best-effort per device.
    pub(in crate::node) async fn publish_profile_to_peers(&self, payload: &ProfilePayload) {
        let wire = payload.encode();
        for peer in self.profile_fanout_targets() {
            self.deliver_profile(&peer, &wire).await;
        }
    }

    /// Re-publish our current profile to a single freshly-discovered `peer`, so a new
    /// contact gets our avatar without waiting for the next time we change it. A no-op if
    /// we've never set an avatar this session (nothing to send).
    pub(in crate::node) async fn republish_profile_to(&self, peer: &PeerRecord) {
        let wire = {
            let guard = self
                .my_profile
                .lock()
                .expect("my_profile mutex not poisoned");
            match guard.as_ref() {
                Some(p) => p.encode(),
                None => return,
            }
        };
        self.deliver_profile(peer, &wire).await;
    }

    /// Every distinct peer DEVICE to publish a profile to: every known peer (any account)
    /// plus our own other devices, never ourselves, deduped by user-id. Unlike a DM
    /// fan-out we don't require a specific target account — a profile goes to everyone we
    /// know so any contact can render it.
    fn profile_fanout_targets(&self) -> Vec<PeerRecord> {
        let me = self.identity.public().user_id();
        let roster = self.roster.lock().expect("roster mutex not poisoned");
        let mut seen = std::collections::HashSet::new();
        roster
            .peers()
            .into_iter()
            .filter(|p| !p.post_office) // a post office relays, it doesn't render avatars
            .filter(|p| p.public.user_id() != me)
            .filter(|p| seen.insert(p.public.user_id()))
            .collect()
    }

    /// Seal `wire` (an encoded `ProfilePayload`) to `peer`'s device via the DM sealed-box,
    /// append it as a `Profile` event to the device-pair conversation, and deliver it
    /// (direct, then post office). Best-effort: transport failures are swallowed (the
    /// event is durably logged and syncs later).
    async fn deliver_profile(&self, peer: &PeerRecord, wire: &[u8]) {
        let conv = dm_conversation_id(&self.identity.public(), &peer.public);
        let sealed = match crate::dm::seal(&self.identity, &peer.public.x25519_pub, wire) {
            Ok(s) => s,
            Err(_) => return,
        };
        if self.append_event(conv, EventKind::Profile, sealed).is_err() {
            return;
        }
        let _ = self.deliver_direct(peer, conv).await;
        let _ = self.replicate_to_post_office(conv).await;
    }

    /// Decode + verify + store any new `Profile` events in `conv` authored by others, then
    /// surface each applied avatar update to the app. A profile is marked emitted only
    /// after it's processed (verified-and-applied, or definitively rejected), so a
    /// transiently-unopenable one (author not yet in the roster) is retried on a later
    /// sync rather than lost — mirroring `emit_new_messages`.
    pub(in crate::node) fn process_profile_events(&self, conv: ConversationId) {
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let candidates: Vec<Event> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            let emitted = self
                .profiles_emitted
                .lock()
                .expect("profiles_emitted mutex not poisoned");
            log.events(&conv)
                .into_iter()
                .filter(|e| {
                    e.kind == EventKind::Profile
                        && e.author != self_author
                        && !emitted.contains(&e.id)
                })
                .cloned()
                .collect()
        };
        for event in candidates {
            let author_uid = event.author.user_id();
            // The author's X25519 (to open the sealed box) + their CERTIFIED account (to
            // bind the profile's claimed account — a device can't forge another account's
            // avatar). Unknown author → retry later (NOT marked emitted).
            let (sender_x25519, peer_account) = {
                let roster = self.roster.lock().expect("roster mutex not poisoned");
                match roster.get(&author_uid) {
                    Some(p) => (p.public.x25519_pub, p.account_id.clone()),
                    None => continue,
                }
            };
            let plaintext = match crate::dm::open(&self.identity, &sender_x25519, &event.ciphertext)
            {
                Ok(p) => p,
                Err(_) => continue, // not yet openable / not a profile we can read — retry later
            };
            // From here the event is readable; mark it emitted regardless of outcome so a
            // malformed/forged/stale profile isn't reprocessed every sync.
            self.profiles_emitted
                .lock()
                .expect("profiles_emitted mutex not poisoned")
                .insert(event.id);
            let Some(payload) = ProfilePayload::decode(&plaintext) else {
                continue;
            };
            // Authenticated: the account signature must verify (also enforces the size
            // bound) AND the claimed account must match the AUTHENTICATED device's
            // certified account (re-homing/spoof defense).
            if !payload.verify() || peer_account.as_deref() != Some(payload.account_id().as_str()) {
                continue;
            }
            // Newest-wins, durable. A stale update returns false (don't surface it).
            let applied = self
                .profiles
                .lock()
                .expect("profiles mutex not poisoned")
                .record(
                    &payload.account_id(),
                    payload.updated_at,
                    payload.avatar.clone(),
                )
                .unwrap_or(false);
            if applied {
                let _ = self.profile_incoming.send(ReceivedProfile {
                    account_id: payload.account_id(),
                    avatar: payload.avatar,
                });
            }
        }
    }
}
