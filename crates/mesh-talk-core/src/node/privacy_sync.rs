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
        let events: Vec<_> = self
            .node
            .log
            .lock()
            .expect("log lock not poisoned")
            .events(conversation)
            .into_iter()
            .cloned()
            .collect();
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
        self.visible(conversation)
            .into_iter()
            .filter(|e| !have.contains(&e.id))
            .collect()
    }
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
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
        self.node
            .log
            .lock()
            .expect("log lock not poisoned")
            .append(event)
    }
}
