//! Per-principal, dependency-closed projection over the actual durable node log.
use super::privacy_runtime::PrivacyState;
use super::*;
use crate::{
    eventlog::{
        AppendOutcome, ConversationId, Event, EventId, EventKind, EventLog, LogError,
        sync::SyncStore,
    },
    identity::device::PublicIdentity,
};
use std::collections::{HashMap, HashSet};

// The staging area is scoped to one authenticated sync connection. An unrelated
// group can consume at most this much memory and is forgotten when it disconnects.
const MAX_PENDING_CHANNEL_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
struct PendingChannel {
    log: EventLog,
    events: Vec<Event>,
    bytes: usize,
}

pub(in crate::node) struct GuardedSyncStore<'a> {
    node: &'a Node,
    peer: PublicIdentity,
    denied: bool,
    pull_only: bool,
    inbound: bool,
    pending_channels: HashMap<ConversationId, PendingChannel>,
    pending_bytes: usize,
}
impl Node {
    pub(in crate::node) fn sync_store(
        &self,
        peer: &PublicIdentity,
    ) -> std::sync::Mutex<GuardedSyncStore<'_>> {
        std::sync::Mutex::new(GuardedSyncStore {
            node: self,
            peer: peer.clone(),
            denied: false,
            pull_only: false,
            inbound: false,
            pending_channels: HashMap::new(),
            pending_bytes: 0,
        })
    }
    pub(in crate::node) fn pull_sync_store(
        &self,
        peer: &PublicIdentity,
    ) -> std::sync::Mutex<GuardedSyncStore<'_>> {
        std::sync::Mutex::new(GuardedSyncStore {
            node: self,
            peer: peer.clone(),
            denied: false,
            pull_only: true,
            inbound: false,
            pending_channels: HashMap::new(),
            pending_bytes: 0,
        })
    }

    pub(in crate::node) fn serve_sync_store(
        &self,
        peer: &PublicIdentity,
    ) -> std::sync::Mutex<GuardedSyncStore<'_>> {
        std::sync::Mutex::new(GuardedSyncStore {
            node: self,
            peer: peer.clone(),
            denied: false,
            pull_only: false,
            inbound: true,
            pending_channels: HashMap::new(),
            pending_bytes: 0,
        })
    }

    pub(in crate::node) fn shares_replica(&self, conversation: &ConversationId) -> bool {
        let me = self.user_id();
        let book = self.channels.lock().expect("channels lock not poisoned");
        if book.state(conversation).is_some() {
            return book.replica_enabled(*conversation, &me);
        }
        drop(book);
        let manifests = self
            .files
            .lock()
            .expect("files lock not poisoned")
            .manifest_events(*conversation);
        let parents: Vec<_> = {
            let log = self.log.lock().expect("log lock not poisoned");
            manifests
                .iter()
                .filter_map(|id| log.get(id).map(|event| event.conversation_id))
                .collect()
        };
        let book = self.channels.lock().expect("channels lock not poisoned");
        parents
            .into_iter()
            .all(|parent| book.state(&parent).is_none() || book.replica_enabled(parent, &me))
    }
}
impl GuardedSyncStore<'_> {
    /// Hold an unknown channel's ancestry off disk until a signed, causally
    /// valid membership snapshot explicitly invites this device. The ordinary
    /// append gate is used only after the complete proof has been projected.
    fn stage_uninvited_channel(
        &mut self,
        event: &Event,
    ) -> Option<Result<AppendOutcome, LogError>> {
        let conversation = event.conversation_id;
        if !self.pending_channels.contains_key(&conversation) {
            if event.kind != EventKind::MembershipChange || !event.parents.is_empty() {
                return None;
            }
            let meta = crate::channel::ChannelMeta::decode(&event.ciphertext)?;
            let me = self.node.identity.public();
            if meta.is_member(&me.user_id())
                || meta.owner != event.author.user_id()
                || !meta.is_member(&meta.owner)
                || !self
                    .node
                    .log
                    .lock()
                    .expect("log lock not poisoned")
                    .events(&conversation)
                    .is_empty()
            {
                return None;
            }
            let guard = self
                .node
                .privacy
                .state
                .read()
                .expect("privacy lock not poisoned");
            let state = guard
                .as_ref()
                .filter(|state| state.policy.snapshot().invisible)?;
            let manual = state
                .policy
                .snapshot()
                .allowed_accounts
                .iter()
                .any(|account| {
                    account.source == PermissionSource::Manual
                        && state
                            .proofs
                            .by_author(&self.peer.ed25519_pub)
                            .is_some_and(|proof| {
                                proof.public() == self.peer
                                    && proof.account_id().as_deref() == Some(&account.id)
                            })
                });
            if !manual {
                return None;
            }
        }

        let mut pending = self
            .pending_channels
            .remove(&conversation)
            .unwrap_or_default();
        self.pending_bytes -= pending.bytes;
        let size = event
            .ciphertext
            .len()
            .saturating_add(event.sig.len())
            .saturating_add(event.parents.len().saturating_mul(32))
            .saturating_add(256);
        if self
            .pending_bytes
            .saturating_add(pending.bytes)
            .saturating_add(size)
            > MAX_PENDING_CHANNEL_BYTES
        {
            self.denied = true;
            return Some(Err(LogError::Io(std::io::Error::new(
                std::io::ErrorKind::OutOfMemory,
                "uninvited channel staging limit exceeded",
            ))));
        }
        match pending.log.append(event.clone()) {
            Ok(AppendOutcome::Duplicate) => {
                self.pending_bytes += pending.bytes;
                self.pending_channels.insert(conversation, pending);
                return Some(Ok(AppendOutcome::Duplicate));
            }
            Err(error) => {
                self.pending_bytes += pending.bytes;
                self.pending_channels.insert(conversation, pending);
                return Some(Err(error));
            }
            Ok(AppendOutcome::Appended) => {}
        }
        pending.bytes += size;
        pending.events.push(event.clone());

        let approved = {
            let guard = self
                .node
                .privacy
                .state
                .read()
                .expect("privacy lock not poisoned");
            guard
                .as_ref()
                .filter(|state| state.policy.snapshot().invisible)
                .and_then(|state| {
                    let invitation = event.kind == EventKind::MembershipChange
                        && crate::channel::ChannelMeta::is_invitation(&event.ciphertext)
                        && crate::channel::ChannelMeta::decode(&event.ciphertext)
                            .is_some_and(|meta| meta.is_member(&self.node.user_id()));
                    if !invitation {
                        return None;
                    }
                    let allowed = self.projection(state, &conversation, &pending.events);
                    allowed.contains(&event.id).then_some(allowed)
                })
        };
        if let Some(allowed) = approved {
            // The proof itself and every committed ancestor have passed both the
            // event-log integrity check and the channel membership projection.
            let guard = self
                .node
                .privacy
                .state
                .read()
                .expect("privacy lock not poisoned");
            let still_allowed = guard
                .as_ref()
                .filter(|state| state.policy.snapshot().invisible)
                .is_some_and(|state| {
                    self.projection(state, &conversation, &pending.events)
                        .contains(&event.id)
                });
            if !still_allowed {
                self.denied = true;
                return Some(Err(LogError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "channel invitation permission changed",
                ))));
            }
            let accepted: Vec<_> = pending
                .events
                .into_iter()
                .filter(|staged| allowed.contains(&staged.id))
                .collect();
            let result = self
                .node
                .log
                .lock()
                .expect("log lock not poisoned")
                .append_channel_batch_atomic(&accepted)
                .map(|()| AppendOutcome::Appended);
            if result.is_err() {
                self.denied = true;
            }
            return Some(result);
        }
        self.pending_bytes += pending.bytes;
        self.pending_channels.insert(conversation, pending);
        Some(Ok(AppendOutcome::Appended))
    }

    fn projection(
        &self,
        state: &PrivacyState,
        conversation: &ConversationId,
        events: &[Event],
    ) -> HashSet<EventId> {
        let me = self.node.identity.public();
        let own_account = self.node.account_id();
        let peer_proof = state
            .proofs
            .by_author(&self.peer.ed25519_pub)
            .filter(|a| a.public() == self.peer);
        let peer_account = peer_proof.as_ref().and_then(|a| a.account_id());
        let own_peer = peer_account.as_deref() == Some(own_account.as_str());
        let relay = peer_proof.as_ref().is_some_and(|a| a.post_office)
            && peer_account.as_ref().is_some_and(|id| {
                state
                    .policy
                    .snapshot()
                    .allowed_accounts
                    .iter()
                    .any(|a| &a.id == id && a.source == PermissionSource::Manual)
            });
        let proofs = state.proofs.announcements();
        let target = proofs.iter().find(|a| {
            a.account_id()
                .is_some_and(|id| id == own_account || state.policy.allows(&id))
                && super::conversation::dm_conversation_id(&me, &a.public()) == *conversation
        });
        let mut allowed = HashSet::new();
        let control_target = proofs.iter().find(|a| {
            a.account_id()
                .is_some_and(|id| id != own_account && state.policy.allows(&id))
                && super::delivery_receipt::delivery_conversation_id(&me, &a.public())
                    == *conversation
        });
        if let Some(target) = control_target {
            if self.peer == target.public() || relay {
                let own_controls = self
                    .node
                    .delivery_control_ids
                    .lock()
                    .expect("control ids lock not poisoned");
                for event in events {
                    let eligible = (event.author.ed25519_pub() == &me.ed25519_pub
                        && own_controls.get(&event.id).is_some_and(|b| {
                            b.device == target.public() && b.account == target.account_id()
                        }))
                        || super::delivery_receipt::open_receipt(
                            &self.node.identity,
                            &own_account,
                            target,
                            event,
                        )
                        .is_some();
                    if event.kind == EventKind::Message
                        && eligible
                        && event.parents.iter().all(|id| allowed.contains(id))
                    {
                        allowed.insert(event.id);
                    }
                }
            }
            return allowed;
        }
        if let Some(target) = target {
            if self.peer == target.public() || own_peer || relay {
                for event in events {
                    let author = event.author.ed25519_pub();
                    if (*author == me.ed25519_pub || *author == target.ed25519_pub)
                        && !matches!(
                            event.kind,
                            EventKind::MembershipChange
                                | EventKind::KeyRotation
                                | EventKind::ChannelRename
                                | EventKind::ChannelOwnerTransfer
                                | EventKind::ChannelLeave
                                | EventKind::ChannelAccept
                        )
                        && event.parents.iter().all(|id| allowed.contains(id))
                    {
                        allowed.insert(event.id);
                    }
                }
            }
            return allowed;
        }
        // A configured private post office may relay a receipt between two
        // other explicitly permitted principals. Group membership confers no
        // permission here; ciphertext remains opaque to the relay.
        let manual_accounts: HashSet<_> = state
            .policy
            .snapshot()
            .allowed_accounts
            .into_iter()
            .filter(|a| a.source == PermissionSource::Manual)
            .map(|a| a.id)
            .collect();
        let manual = |account: &Option<String>| {
            account
                .as_ref()
                .is_some_and(|id| manual_accounts.contains(id))
        };
        if state.own.post_office
            && manual(&peer_account)
            && let Some(other) = proofs.iter().find(|a| {
                a.public() != self.peer
                    && manual(&a.account_id())
                    && super::delivery_receipt::delivery_conversation_id(&self.peer, &a.public())
                        == *conversation
            })
        {
            for event in events {
                let author = event.author.ed25519_pub();
                if (*author == self.peer.ed25519_pub || *author == other.ed25519_pub)
                    && event.kind == EventKind::Message
                    && super::delivery_receipt::valid_sealed_wire(&event.ciphertext)
                    && event.parents.iter().all(|id| allowed.contains(id))
                {
                    allowed.insert(event.id);
                }
            }
            return allowed;
        }
        // A file conversation inherits its verified manifest's parent scope.
        if let Some(scopes) = state.file_scopes.get(conversation) {
            let mut authors = HashSet::new();
            for scope in scopes {
                let parent_events: Vec<_> = self
                    .node
                    .log
                    .lock()
                    .expect("log lock not poisoned")
                    .events(&scope.parent)
                    .into_iter()
                    .cloned()
                    .collect();
                if scope.parent != *conversation
                    && !state.file_scopes.contains_key(&scope.parent)
                    && self
                        .projection(state, &scope.parent, &parent_events)
                        .contains(&scope.event)
                {
                    authors.insert(scope.author);
                }
            }
            for event in events {
                if event.kind == EventKind::Message
                    && authors.contains(&event.author)
                    && event.parents.iter().all(|id| allowed.contains(id))
                {
                    allowed.insert(event.id);
                }
            }
            return allowed;
        }
        self.channel_projection(conversation, events, relay, own_peer, Some(state))
    }

    /// Signed channel authority applies in public and invisible presence modes.
    /// Public presence does not make a private group's event log public.
    fn channel_projection(
        &self,
        conversation: &ConversationId,
        events: &[Event],
        relay: bool,
        own_peer: bool,
        private_state: Option<&PrivacyState>,
    ) -> HashSet<EventId> {
        #[cfg(test)]
        self.node
            .channel_projection_replays
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let me = self.node.identity.public();
        let mut allowed = HashSet::new();
        // Established channel metadata pins its owner. Historical membership only
        // authorizes that channel's history; it never grants DM/account permission.
        let established = {
            let book = self
                .node
                .channels
                .lock()
                .expect("channels lock not poisoned");
            book.state(conversation)
                .map(|s| (s.genesis_owner().to_string(), s.members().to_vec()))
        };
        let by_id: HashMap<_, _> = events.iter().map(|event| (event.id, event)).collect();
        // A newly invited device is absent from the genesis snapshot. Validate
        // the later membership chain before projecting its metadata ancestors;
        // otherwise dependency closure prevents the invite event from syncing.
        let future_pair = {
            let mut future: Option<crate::channel::ChannelState> = None;
            let mut future_departures: Vec<(String, EventId)> = Vec::new();
            for event in events {
                let author = event.author.user_id();
                match event.kind {
                    EventKind::MembershipChange => {
                        if let Some(mut meta) =
                            crate::channel::ChannelMeta::decode(&event.ciphertext)
                        {
                            let original_member_count = meta.members.len();
                            meta.members.retain(|member| {
                                !future_departures.iter().any(|(uid, leave_id)| {
                                    uid == &member.user_id()
                                        && !super::channel::descends_from(event, *leave_id, &by_id)
                                })
                            });
                            if let Some(current) = future.as_mut() {
                                if author == current.owner() {
                                    let old: HashSet<_> = current
                                        .members()
                                        .iter()
                                        .map(PublicIdentity::user_id)
                                        .collect();
                                    let invited = crate::channel::ChannelMeta::is_invitation(
                                        &event.ciphertext,
                                    );
                                    if meta.members.len() < original_member_count {
                                        current.apply_meta_after_concurrent_leave(meta);
                                    } else {
                                        current.apply_meta(meta);
                                    }
                                    let added: Vec<_> = current
                                        .members()
                                        .iter()
                                        .map(PublicIdentity::user_id)
                                        .filter(|uid| !old.contains(uid))
                                        .collect();
                                    for uid in added {
                                        if invited {
                                            current.mark_invited(&uid);
                                        } else {
                                            current.auto_accept_legacy_member(&uid);
                                        }
                                    }
                                }
                            } else if event.parents.is_empty()
                                && author == meta.owner
                                && meta.is_member(&author)
                                && established
                                    .as_ref()
                                    .is_none_or(|(owner, _)| owner == &author)
                            {
                                let invited =
                                    crate::channel::ChannelMeta::is_invitation(&event.ciphertext);
                                let mut initial =
                                    crate::channel::ChannelState::from_meta(*conversation, meta);
                                if invited {
                                    let invitees: Vec<_> = initial
                                        .members()
                                        .iter()
                                        .map(PublicIdentity::user_id)
                                        .filter(|uid| uid != &author)
                                        .collect();
                                    for uid in invitees {
                                        initial.mark_invited(&uid);
                                    }
                                }
                                future = Some(initial);
                            }
                        }
                    }
                    EventKind::ChannelOwnerTransfer => {
                        if let Some(current) = future.as_mut()
                            && author == current.owner()
                            && let Ok(next) = std::str::from_utf8(&event.ciphertext)
                        {
                            current.transfer_owner(next);
                        }
                    }
                    EventKind::ChannelAccept => {
                        if event.ciphertext.is_empty()
                            && let Some(current) = future.as_mut()
                        {
                            current.accept_invite(&author, event.id);
                        }
                    }
                    EventKind::ChannelLeave => {
                        if let Some(current) = future.as_mut()
                            && event.ciphertext.is_empty()
                            && current.leave_member(&author)
                        {
                            future_departures.push((author, event.id));
                        }
                    }
                    _ => {}
                }
            }
            future.is_some_and(|current| {
                current.is_member(&me.user_id())
                    && (current.is_member(&self.peer.user_id()) || relay)
            })
        };
        let mut membership: Option<crate::channel::ChannelState> = None;
        let mut dissolved = false;
        let mut departed_actor: Option<String> = None;
        let mut departures: Vec<(String, EventId)> = Vec::new();
        let mut removed_self = false;
        for event in events {
            if dissolved {
                continue;
            }
            if !event.parents.iter().all(|id| allowed.contains(id)) {
                continue;
            }
            if removed_self && event.kind != EventKind::MembershipChange {
                continue;
            }
            let author = event.author.user_id();
            if departed_actor.is_some()
                && !matches!(
                    event.kind,
                    EventKind::MembershipChange
                        | EventKind::ChannelOwnerTransfer
                        | EventKind::ChannelAccept
                )
            {
                continue;
            }
            let accepted = if event.kind == EventKind::MembershipChange {
                if let Some(mut meta) = crate::channel::ChannelMeta::decode(&event.ciphertext) {
                    let original_member_count = meta.members.len();
                    meta.members.retain(|member| {
                        !departures.iter().any(|(uid, leave_id)| {
                            uid == &member.user_id()
                                && !super::channel::descends_from(event, *leave_id, &by_id)
                        })
                    });
                    if let Some(current) = membership.as_mut() {
                        if author == current.owner()
                            && meta.owner == current.owner()
                            && (!removed_self || meta.is_member(&me.user_id()))
                        {
                            let was_member = current.is_member(&me.user_id());
                            let prior: HashSet<_> = current
                                .members()
                                .iter()
                                .map(PublicIdentity::user_id)
                                .collect();
                            let invited =
                                crate::channel::ChannelMeta::is_invitation(&event.ciphertext);
                            if meta.members.len() < original_member_count {
                                current.apply_meta_after_concurrent_leave(meta);
                            } else {
                                current.apply_meta(meta);
                            }
                            let added: Vec<_> = current
                                .members()
                                .iter()
                                .map(PublicIdentity::user_id)
                                .filter(|uid| !prior.contains(uid))
                                .collect();
                            for uid in added {
                                if invited {
                                    current.mark_invited(&uid);
                                } else {
                                    current.auto_accept_legacy_member(&uid);
                                }
                            }
                            removed_self =
                                (removed_self || was_member) && !current.is_member(&me.user_id());
                            if departed_actor
                                .as_ref()
                                .is_some_and(|id| current.is_member(id))
                            {
                                departed_actor = None;
                            }
                            true
                        } else {
                            false
                        }
                    } else {
                        let pinned = established
                            .as_ref()
                            .is_some_and(|(owner, _)| owner == &author);
                        let owner_allowed = private_state.is_none_or(|state| {
                            state.proofs.announcements().iter().any(|a| {
                                a.ed25519_pub == *event.author.ed25519_pub()
                                    && a.account_id().is_some_and(|id| {
                                        id == self.node.account_id() || state.policy.allows(&id)
                                    })
                            })
                        });
                        if event.parents.is_empty()
                            && meta.owner == author
                            && meta.is_member(&author)
                            && (meta.is_member(&me.user_id()) || future_pair)
                            && if established.is_some() {
                                pinned
                            } else {
                                owner_allowed
                            }
                        {
                            let invited =
                                crate::channel::ChannelMeta::is_invitation(&event.ciphertext);
                            let mut initial =
                                crate::channel::ChannelState::from_meta(*conversation, meta);
                            if invited {
                                let invitees: Vec<_> = initial
                                    .members()
                                    .iter()
                                    .map(PublicIdentity::user_id)
                                    .filter(|uid| uid != &author)
                                    .collect();
                                for uid in invitees {
                                    initial.mark_invited(&uid);
                                }
                            }
                            membership = Some(initial);
                            true
                        } else {
                            false
                        }
                    }
                } else {
                    false
                }
            } else if event.kind == EventKind::ChannelOwnerTransfer {
                membership.as_mut().is_some_and(|current| {
                    author == current.owner()
                        && std::str::from_utf8(&event.ciphertext)
                            .ok()
                            .is_some_and(|next| current.transfer_owner(next))
                })
            } else if event.kind == EventKind::ChannelLeave {
                membership.as_mut().is_some_and(|current| {
                    let leaving = event.ciphertext.is_empty() && current.leave_member(&author);
                    if leaving {
                        departures.push((author.clone(), event.id));
                    }
                    if leaving && (author == me.user_id() || author == self.peer.user_id()) {
                        departed_actor = Some(author.clone());
                    }
                    leaving
                })
            } else if event.kind == EventKind::ChannelAccept {
                if event.ciphertext.is_empty() {
                    membership.as_mut().is_some_and(|current| {
                        if current.is_member(&author) {
                            current.accept_invite(&author, event.id);
                            true
                        } else {
                            false
                        }
                    })
                } else {
                    false
                }
            } else {
                membership.as_ref().is_some_and(|s| {
                    s.is_accepted_member(&author)
                        && (!matches!(
                            event.kind,
                            EventKind::ChannelRename | EventKind::ChannelDissolve
                        ) || author == s.owner())
                        && (event.kind != EventKind::ChannelDissolve || event.ciphertext.is_empty())
                        && event.kind != EventKind::Profile
                })
            };
            if accepted && event.parents.iter().all(|id| allowed.contains(id)) {
                allowed.insert(event.id);
                if event.kind == EventKind::ChannelDissolve {
                    dissolved = true;
                }
            }
        }
        let current = membership
            .as_ref()
            .map(|s| s.members().to_vec())
            .or_else(|| established.as_ref().map(|(_, members)| members.clone()));
        let current_pair = current.as_ref().is_some_and(|members| {
            members.contains(&me) && (members.contains(&self.peer) || relay || own_peer)
        });
        let departure_pair = departed_actor.as_ref().is_some_and(|departed| {
            current.as_ref().is_some_and(|members| {
                (departed == &me.user_id() && members.contains(&self.peer))
                    || (departed == &self.peer.user_id() && members.contains(&me))
            })
        });
        let removal_pair = removed_self
            && current
                .as_ref()
                .is_some_and(|members| members.contains(&self.peer) || relay);
        if !current_pair && !departure_pair && !removal_pair {
            allowed.clear();
        }
        allowed
    }
    fn public_projection_key(
        &self,
        conversation: &ConversationId,
    ) -> Option<super::channel_projection_cache::ProjectionKey> {
        let guard = self
            .node
            .privacy
            .state
            .read()
            .expect("privacy lock not poisoned");
        if guard
            .as_ref()
            .is_some_and(|state| state.policy.snapshot().invisible)
            || self.pending_channels.contains_key(conversation)
        {
            return None;
        }
        let privacy_configured = guard.is_some();
        let privacy_generation = self
            .node
            .privacy
            .generation
            .load(std::sync::atomic::Ordering::SeqCst);
        let sharing = self.node.shares_replica(conversation);
        let established = self
            .node
            .channels
            .lock()
            .expect("channels lock not poisoned")
            .state(conversation)
            .map(|state| {
                use sha2::{Digest, Sha256};
                let mut hash = Sha256::new();
                hash.update(state.genesis_owner().len().to_le_bytes());
                hash.update(state.genesis_owner().as_bytes());
                let mut members = state.members().to_vec();
                members.sort_unstable_by_key(|member| (member.ed25519_pub, member.x25519_pub));
                for member in members {
                    hash.update(member.ed25519_pub);
                    hash.update(member.x25519_pub);
                }
                hash.finalize().into()
            });
        let raw = self
            .node
            .log
            .lock()
            .expect("log lock not poisoned")
            .fingerprint_and_len(conversation);
        Some(super::channel_projection_cache::ProjectionKey {
            conversation: *conversation,
            raw,
            peer: self.peer.clone(),
            own: self.node.identity.public(),
            own_account: self.node.account_id(),
            established,
            privacy_generation,
            privacy_configured,
            inbound: self.inbound,
            sharing,
        })
    }

    fn public_projection_metadata(
        &self,
        conversation: &ConversationId,
    ) -> Option<super::channel_projection_cache::ProjectionMetadata> {
        let key = self.public_projection_key(conversation)?;
        let cached = self
            .node
            .channel_projection_cache
            .lock()
            .expect("projection cache lock not poisoned")
            .get(&key);
        if let Some(metadata) = cached {
            if self.public_projection_key(conversation).as_ref() == Some(&key) {
                return Some(metadata);
            }
            return None;
        }
        // On a miss, detect signed channel ancestry directly in the durable log.
        // ChannelBook may lag behind the append; it is not the public authority.
        let channel = key.established.is_some()
            || self
                .node
                .log
                .lock()
                .expect("log lock not poisoned")
                .events(conversation)
                .iter()
                .any(|event| event.kind == EventKind::MembershipChange);
        if !channel {
            return None;
        }
        let visible = self.visible(conversation);
        let metadata = super::channel_projection_cache::ProjectionMetadata {
            fingerprint: crate::eventlog::sync::fingerprint(
                visible.iter().map(|event| event.id).collect(),
            ),
            len: visible.len(),
            requires_v2: visible
                .iter()
                .any(|event| event.kind == EventKind::ChannelReplica),
        };
        if self.public_projection_key(conversation).as_ref() != Some(&key) {
            return None;
        }
        self.node
            .channel_projection_cache
            .lock()
            .expect("projection cache lock not poisoned")
            .insert(key, metadata);
        Some(metadata)
    }

    fn visible(&self, conversation: &ConversationId) -> Vec<Event> {
        if self.inbound && !self.node.shares_replica(conversation) {
            return Vec::new();
        }
        let guard = self
            .node
            .privacy
            .state
            .read()
            .expect("privacy lock not poisoned");
        let mut events: Vec<_> = self
            .node
            .log
            .lock()
            .expect("log lock not poisoned")
            .events(conversation)
            .into_iter()
            .cloned()
            .collect();
        if guard
            .as_ref()
            .is_none_or(|state| !state.policy.snapshot().invisible)
            && (events
                .iter()
                .any(|event| event.kind == EventKind::MembershipChange)
                || self
                    .node
                    .channels
                    .lock()
                    .expect("channels lock not poisoned")
                    .state(conversation)
                    .is_some())
        {
            let allowed = self.channel_projection(conversation, &events, false, false, None);
            return events
                .into_iter()
                .filter(|event| allowed.contains(&event.id))
                .collect();
        }
        // Local erase cancels receipt retries, while preserving encrypted chat
        // history's established non-recall semantics. Apply this in public mode
        // too, so dormant erased controls cannot be advertised again.
        let controls = self
            .node
            .delivery_control_ids
            .lock()
            .expect("control ids lock not poisoned");
        let mut allowed_controls = HashSet::new();
        events.retain(|event| {
            if !super::delivery_receipt::valid_sealed_wire(&event.ciphertext) {
                return true;
            }
            let allowed = (event.author.ed25519_pub() != &self.node.identity.public().ed25519_pub
                || controls.get(&event.id).is_some_and(|binding| {
                    let proof = if let Some(state) = guard.as_ref() {
                        state.proofs.by_author(&binding.device.ed25519_pub)
                    } else {
                        self.node
                            .roster
                            .lock()
                            .expect("roster lock not poisoned")
                            .historical_announcement(&binding.device)
                            .cloned()
                    };
                    proof.is_some_and(|p| {
                        p.public() == binding.device
                            && p.account_id() == binding.account
                            && super::delivery_receipt::delivery_conversation_id(
                                &self.node.identity.public(),
                                &binding.device,
                            ) == event.conversation_id
                    })
                }))
                && event.parents.iter().all(|id| allowed_controls.contains(id));
            if allowed {
                allowed_controls.insert(event.id);
            }
            allowed
        });
        drop(controls);
        let Some(state) = guard.as_ref().filter(|s| s.policy.snapshot().invisible) else {
            return events;
        };
        let allowed = self.projection(state, conversation, &events);
        events
            .into_iter()
            .filter(|e| allowed.contains(&e.id))
            .collect()
    }
}
impl SyncStore for GuardedSyncStore<'_> {
    fn requires_channel_v2(&self, conversation: &ConversationId) -> bool {
        if let Some(metadata) = self.public_projection_metadata(conversation) {
            return metadata.requires_v2;
        }
        // Inspect the peer's projection: a hidden event must not affect the
        // compatibility response or reveal that it exists.
        if !self
            .node
            .log
            .lock()
            .expect("log lock not poisoned")
            .requires_channel_v2(conversation)
        {
            return false;
        }
        self.visible(conversation)
            .iter()
            .any(|event| event.kind == EventKind::ChannelReplica)
    }
    fn fingerprint_and_len(&self, conversation: &ConversationId) -> ([u8; 32], usize) {
        if let Some(metadata) = self.public_projection_metadata(conversation) {
            return (metadata.fingerprint, metadata.len);
        }
        let ids = self.event_ids(conversation);
        let len = ids.len();
        (crate::eventlog::sync::fingerprint(ids), len)
    }
    fn file_consent(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> crate::eventlog::sync::FileConsentStatus {
        use crate::eventlog::sync::FileConsentStatus;
        match self
            .node
            .attachment_consent_for_peer(*file, *manifest, &self.peer)
        {
            super::attachment_consent::ReceiveDecision::Pending => FileConsentStatus::Pending,
            super::attachment_consent::ReceiveDecision::Accepted => FileConsentStatus::Accepted,
            super::attachment_consent::ReceiveDecision::Declined => FileConsentStatus::Declined,
        }
    }
    fn signed_file_consent(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> Option<crate::eventlog::Event> {
        self.node
            .direct_file_consent_control(&self.peer, *file, *manifest)
            .ok()
            .flatten()
    }
    fn durable_have(&self, conversation: &ConversationId, id: &EventId) -> bool {
        if !self.visible(conversation).iter().any(|e| e.id == *id) {
            return !self.denied
                && self
                    .node
                    .historical_file_completion(*conversation, *id, &self.peer);
        }
        self.node
            .log
            .lock()
            .expect("log lock not poisoned")
            .sync()
            .is_ok()
    }
    fn admission_denied(&self) -> bool {
        self.denied
    }
    fn event_ids(&self, conversation: &ConversationId) -> Vec<EventId> {
        let mut ids: Vec<_> = self.visible(conversation).iter().map(|e| e.id).collect();
        if let Some(pending) = self.pending_channels.get(conversation) {
            ids.extend(pending.events.iter().map(|event| event.id));
        }
        ids
    }
    fn events_excluding(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
    ) -> Vec<Event> {
        if self.pull_only {
            return Vec::new();
        }
        let mut events = self.visible(conversation);
        if self.node.certified_sibling(&self.peer)
            && *conversation
                == super::conversation::dm_conversation_id(&self.node.identity.public(), &self.peer)
        {
            let policy = self.node.device_sync_policy();
            let mut available = have.clone();
            events.retain(|event| {
                let selected = match event.kind {
                    EventKind::Message => policy.history,
                    EventKind::FileManifest => policy.attachments,
                    _ => true,
                };
                let allowed = selected
                    && event
                        .parents
                        .iter()
                        .all(|parent| available.contains(parent));
                if allowed {
                    available.insert(event.id);
                }
                allowed
            });
        }
        if !self.inbound && !self.node.shares_replica(conversation) {
            // Share-off applies to outbound retries and explicit sends too.
            // Own records can advance only across parents the receiver already
            // has, or other eligible own records in this same ordered batch.
            // Missing third-party ancestry must come from a sharing holder.
            let mut available = have.clone();
            let own = self.node.identity.public().ed25519_pub;
            return events
                .into_iter()
                .filter(|event| {
                    let eligible = event.author.ed25519_pub() == &own
                        && event
                            .parents
                            .iter()
                            .all(|parent| available.contains(parent));
                    if eligible {
                        available.insert(event.id);
                    }
                    eligible && !have.contains(&event.id)
                })
                .collect();
        }
        events
            .into_iter()
            .filter(|event| !have.contains(&event.id))
            .collect()
    }
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        if let Some(result) = self.stage_uninvited_channel(&event) {
            return result;
        }
        let known_file = {
            self.node
                .files
                .lock()
                .expect("files lock not poisoned")
                .manifest(&event.conversation_id)
                .is_some()
        };
        // A chunk-first legacy sender must not fill the recipient's log before
        // its offer has arrived. Ordinary messages have an authenticated DM,
        // control, or established channel scope; a fresh file conversation has
        // none until its verified manifest has been installed.
        if event.kind == EventKind::Message && !known_file {
            let own = self.node.identity.public();
            let peer = self.node.historical_author(event.author.ed25519_pub());
            let direct = peer.as_ref().is_some_and(|proof| {
                super::conversation::dm_conversation_id(&own, &proof.public())
                    == event.conversation_id
                    || super::delivery_receipt::delivery_conversation_id(&own, &proof.public())
                        == event.conversation_id
            });
            let channel = self
                .node
                .channels
                .lock()
                .expect("channels lock not poisoned")
                .state(&event.conversation_id)
                .is_some()
                || self
                    .node
                    .log
                    .lock()
                    .expect("log lock not poisoned")
                    .events(&event.conversation_id)
                    .iter()
                    .any(|entry| entry.kind == EventKind::MembershipChange);
            let relay = self
                .node
                .privacy
                .state
                .read()
                .expect("privacy lock not poisoned")
                .as_ref()
                .is_some_and(|state| state.own.post_office);
            if !direct && !channel && !relay && event.author.ed25519_pub() != &own.ed25519_pub {
                self.denied = true;
                return Err(LogError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "unknown attachment conversation",
                )));
            }
        }
        // A legacy peer can skip the optional consent query and push chunks through
        // ordinary sync. Enforce the same receive decision at the durable log gate.
        if event.kind == EventKind::Message
            && known_file
            && !self
                .node
                .attachment_chunk_author_allowed(event.conversation_id, event.author.ed25519_pub())
        {
            self.denied = true;
            return Err(LogError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "attachment awaiting recipient approval",
            )));
        }
        if event.kind == EventKind::Message && known_file {
            let manifest = self
                .node
                .files
                .lock()
                .expect("files lock not poisoned")
                .manifest(&event.conversation_id)
                .cloned();
            if let Some(manifest) = manifest.filter(super::media_store::automatic_inline_image) {
                let first_chunk = self
                    .node
                    .log
                    .lock()
                    .expect("log lock not poisoned")
                    .events(&event.conversation_id)
                    .is_empty();
                if first_chunk && (event.seq != 1 || !event.parents.is_empty()) {
                    self.denied = true;
                    return Err(LogError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "automatic image must start at chunk one",
                    )));
                }
                if event.seq == 1 {
                    let valid = crate::file::open_chunk_for(&manifest, 0, &event.ciphertext)
                        .ok()
                        .is_some_and(|bytes| {
                            super::media_store::raster_signature_matches(manifest.mime(), &bytes)
                        });
                    if !valid {
                        self.denied = true;
                        return Err(LogError::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "automatic image has invalid image signature",
                        )));
                    }
                }
            }
        }
        // Own-author backfill may compete with an accepted local reservation.
        // Delivery precedes privacy/log locks, matching enqueue's lock order.
        let mut delivery = if event.author.ed25519_pub() == &self.node.identity.public().ed25519_pub
        {
            Some(
                self.node
                    .delivery
                    .lock()
                    .expect("delivery lock not poisoned"),
            )
        } else {
            None
        };
        if let Some(store) = delivery.as_mut() {
            self.node.recover_delivery(store)?;
        }
        let guard = self
            .node
            .privacy
            .state
            .read()
            .expect("privacy lock not poisoned");
        if let Some(state) = guard.as_ref().filter(|s| s.policy.snapshot().invisible) {
            let mut events: Vec<_> = self
                .node
                .log
                .lock()
                .expect("log lock not poisoned")
                .events(&event.conversation_id)
                .into_iter()
                .cloned()
                .collect();
            events.push(event.clone());
            events.sort_by_key(|e| (e.lamport, e.id));
            if !self
                .projection(state, &event.conversation_id, &events)
                .contains(&event.id)
            {
                self.denied = true;
                return Err(LogError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "event admission denied",
                )));
            }
        }
        let mut log = self.node.log.lock().expect("log lock not poisoned");
        if event.kind == EventKind::Message
            && known_file
            && log
                .events(&event.conversation_id)
                .iter()
                .any(|chunk| chunk.author != event.author)
        {
            self.denied = true;
            return Err(LogError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "attachment chunk stream already belongs to another sender",
            )));
        }
        if matches!(event.kind, EventKind::Message | EventKind::FileManifest) {
            log.append_durable(event)
        } else {
            log.append(event)
        }
    }
}
