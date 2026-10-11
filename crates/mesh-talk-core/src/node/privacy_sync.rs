//! Per-principal, dependency-closed projection over the actual durable node log.
use super::privacy_runtime::PrivacyState;
use super::*;
use crate::{
    eventlog::{
        sync::SyncStore, AppendOutcome, ConversationId, Event, EventId, EventKind, LogError,
    },
    identity::device::PublicIdentity,
};
use std::collections::HashSet;

pub(in crate::node) struct GuardedSyncStore<'a> {
    node: &'a Node,
    peer: PublicIdentity,
    denied: bool,
    pull_only: bool,
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
        })
    }
}
impl GuardedSyncStore<'_> {
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
        if state.own.post_office && manual(&peer_account) {
            if let Some(other) = proofs.iter().find(|a| {
                a.public() != self.peer
                    && manual(&a.account_id())
                    && super::delivery_receipt::delivery_conversation_id(&self.peer, &a.public())
                        == *conversation
            }) {
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
        // Established channel metadata pins its owner. Historical membership only
        // authorizes that channel's history; it never grants DM/account permission.
        let established = {
            let book = self
                .node
                .channels
                .lock()
                .expect("channels lock not poisoned");
            book.state(conversation)
                .map(|s| (s.owner().to_string(), s.members().to_vec()))
        };
        let mut membership: Option<crate::channel::ChannelState> = None;
        for event in events {
            if !event.parents.iter().all(|id| allowed.contains(id)) {
                continue;
            }
            let author = event.author.user_id();
            let accepted = if event.kind == EventKind::MembershipChange {
                if let Some(meta) = crate::channel::ChannelMeta::decode(&event.ciphertext) {
                    if let Some(current) = membership.as_mut() {
                        if author == current.owner() && meta.owner == current.owner() {
                            current.apply_meta(meta);
                            true
                        } else {
                            false
                        }
                    } else {
                        let pinned = established
                            .as_ref()
                            .is_some_and(|(owner, _)| owner == &author);
                        let owner_allowed = proofs.iter().any(|a| {
                            a.ed25519_pub == *event.author.ed25519_pub()
                                && a.account_id()
                                    .is_some_and(|id| id == own_account || state.policy.allows(&id))
                        });
                        if event.parents.is_empty()
                            && meta.owner == author
                            && meta.is_member(&author)
                            && meta.is_member(&me.user_id())
                            && (pinned || owner_allowed)
                        {
                            membership =
                                Some(crate::channel::ChannelState::from_meta(*conversation, meta));
                            true
                        } else {
                            false
                        }
                    }
                } else {
                    false
                }
            } else {
                membership.as_ref().is_some_and(|s| {
                    s.members()
                        .iter()
                        .any(|p| p.ed25519_pub == *event.author.ed25519_pub())
                        && (event.kind != EventKind::ChannelRename || author == s.owner())
                        && event.kind != EventKind::Profile
                })
            };
            if accepted && event.parents.iter().all(|id| allowed.contains(id)) {
                allowed.insert(event.id);
            }
        }
        let current = membership
            .as_ref()
            .map(|s| s.members().to_vec())
            .or_else(|| established.as_ref().map(|(_, members)| members.clone()));
        if !current.is_some_and(|members| {
            members.contains(&me) && (members.contains(&self.peer) || relay || own_peer)
        }) {
            allowed.clear();
        }
        allowed
    }
    fn visible(&self, conversation: &ConversationId) -> Vec<Event> {
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
        self.visible(conversation).iter().map(|e| e.id).collect()
    }
    fn events_excluding(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
    ) -> Vec<Event> {
        if self.pull_only {
            return Vec::new();
        }
        self.visible(conversation)
            .into_iter()
            .filter(|e| !have.contains(&e.id))
            .collect()
    }
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
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
