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
mod network;
mod principal;

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

    pub(super) fn file_identity_endpoint_key(
        &self,
        addr: std::net::SocketAddr,
        peer: &PublicIdentity,
    ) -> super::session_pool::PeerEndpointKey {
        let announcement = self
            .roster
            .lock()
            .expect("roster lock")
            .announcement(peer)
            .cloned();
        let candidate = self
            .privacy_route_candidates(addr, peer, announcement.as_ref())
            .first()
            .copied()
            .unwrap_or(addr);
        self.peer_endpoint_key(candidate, peer)
    }

    pub(super) fn file_identity_capability_due(
        &self,
        addr: std::net::SocketAddr,
        peer: &PublicIdentity,
    ) -> bool {
        self.sessions
            .file_identity_capability_due(&self.file_identity_endpoint_key(addr, peer))
    }

    /// Missing-only bounded catch-up repairs retained legacy manifests without heuristics.
    pub(super) fn pending_file_message_identities(
        &self,
        peer: &PublicIdentity,
    ) -> Result<Vec<(ConversationId, EventId)>, LogError> {
        let sibling = self.certified_sibling(peer);
        let peer_host = self
            .historical_author(&peer.ed25519_pub)
            .and_then(|proof| proof.account_id())
            .map(|account| {
                super::conversation::account_conversation_id(&self.account_id(), &account)
            });
        let after = self.sessions.file_identity_scan_after(peer);
        let page = self
            .received_files
            .lock()
            .expect("received files lock")
            .scan_page(
                None,
                after,
                super::transcript_repository::TranscriptPageLimit::new(16, 8 * 1024 * 1024)?,
            )?;
        let personal_host =
            super::conversation::account_conversation_id(&self.account_id(), &self.account_id());
        let mut candidates = Vec::new();
        for row in page.rows {
            if !(sibling || row.from == peer.user_id() && Some(row.conversation) == peer_host)
                || row.conversation == personal_host
                || self
                    .channels
                    .lock()
                    .expect("channels lock")
                    .state(&row.conversation)
                    .is_some()
                || self.has_file_message_identity(row.conversation, row.event_id)?
                || self.recipient_expired(&row.plaintext, row.event_id, row.wall_clock)
            {
                continue;
            }
            let logical = self.file_message_row_logical_id(&row)?;
            let delivery = self.delivery.lock().expect("delivery lock");
            if delivery.file_erased(row.conversation, row.event_id)
                || delivery.manifest_event_erased(row.event_id)
            {
                continue;
            }
            let suppression = self.account_suppression.lock().expect("suppression lock");
            if suppression.contains(row.conversation, row.event_id)?
                || suppression.contains(row.conversation, logical)?
            {
                continue;
            }
            if let Some(manifest) = super::files::validated_manifest(&row.plaintext) {
                candidates.push((manifest.file_conv(), row.event_id));
            }
        }
        if !candidates.is_empty() {
            let offset = self.sessions.file_identity_scan_offset(
                peer,
                2,
                candidates.len(),
                super::session::MAX_FILE_IDENTITIES,
            );
            candidates.rotate_left(offset);
            candidates.truncate(super::session::MAX_FILE_IDENTITIES);
        }
        self.sessions.advance_file_identity_scan(peer, page.next);
        Ok(candidates)
    }

    pub(super) fn file_message_identity_controls_for(
        &self,
        peer: &PublicIdentity,
        missing_only: bool,
    ) -> Result<Vec<Event>, LogError> {
        let manifests: Vec<_> = {
            let delivery = self.delivery.lock().expect("delivery lock");
            let total = delivery.file_cards().count();
            let offset = self.sessions.file_identity_scan_offset(
                peer,
                1,
                total,
                super::session::MAX_FILE_IDENTITIES,
            );
            delivery
                .file_cards()
                .skip(offset)
                .take(super::session::MAX_FILE_IDENTITIES)
                .flat_map(|card| {
                    card.destinations
                        .iter()
                        .filter(|destination| destination.binding.device == *peer)
                        .map(|destination| {
                            (
                                card.conversation,
                                card.file_conversation,
                                destination.binding.event_id,
                            )
                        })
                })
                .collect()
        };
        let store = self.sync_store(peer);
        let mut selected = Vec::new();
        for (host, file, manifest) in manifests {
            if missing_only && self.has_file_message_identity(host, manifest)? {
                continue;
            }
            if let Some(event) = store
                .lock()
                .expect("sync store lock")
                .file_message_identity(&file, &manifest)?
            {
                selected.push(event);
            }
            if selected.len() == super::session::MAX_FILE_IDENTITIES {
                break;
            }
        }
        Ok(selected)
    }

    /// Exchange supplemental identities without changing existing media/history bytes.
    pub(super) async fn exchange_file_message_identities(
        &self,
        channel: &mut crate::transport::SecureChannel<tokio::net::TcpStream>,
        requests: Vec<(ConversationId, EventId)>,
        offers: Vec<Event>,
    ) -> Result<(), super::session::SessionError> {
        self.exchange_file_message_identities_at_revision(channel, requests, offers, None)
            .await
    }

    pub(super) async fn exchange_file_message_identities_at_revision(
        &self,
        channel: &mut crate::transport::SecureChannel<tokio::net::TcpStream>,
        requests: Vec<(ConversationId, EventId)>,
        offers: Vec<Event>,
        revision: Option<u64>,
    ) -> Result<(), super::session::SessionError> {
        let peer = channel.peer_identity().clone();
        let key = self.peer_endpoint_key(
            channel
                .tcp_peer_addr()
                .map_err(crate::transport::TransportError::Io)?,
            &peer,
        );
        if !self.sessions.file_identity_capability_due(&key) {
            return Ok(());
        }
        let bundles = match super::session::request_file_message_identities(
            channel,
            requests.clone(),
            offers,
        )
        .await
        {
            Ok(bundles) => {
                self.sessions.note_file_identity_supported(&key);
                bundles
            }
            Err(error) => {
                self.sessions.note_file_identity_unsupported(key);
                return Err(error);
            }
        };
        self.accept_file_message_identity_response(&peer, &requests, bundles, revision)
    }

    pub(super) fn accept_file_message_identity_response(
        &self,
        peer: &PublicIdentity,
        requests: &[(ConversationId, EventId)],
        bundles: Vec<Event>,
        revision: Option<u64>,
    ) -> Result<(), super::session::SessionError> {
        let _commit = revision.map(|_| {
            self.account_sync_commit
                .lock()
                .expect("account sync commit lock")
        });
        if revision.is_some_and(|revision| self.device_sync_policy_snapshot().1 != revision) {
            return Err(super::session::SessionError::UnexpectedMessage);
        }
        let store = self.sync_store(peer);
        for bundle in bundles {
            if !requests.iter().any(|(file, manifest)| {
                super::file_message_identity::bundle_covers(&bundle, *file, *manifest)
            }) {
                return Err(super::session::SessionError::UnexpectedMessage);
            }
            if !store
                .lock()
                .expect("sync store lock")
                .accept_file_message_identity(bundle)?
            {
                return Err(super::session::SessionError::UnexpectedMessage);
            }
        }
        Ok(())
    }

    pub(super) async fn offer_file_message_identity(
        &self,
        channel: &mut crate::transport::SecureChannel<tokio::net::TcpStream>,
        manifest: EventId,
    ) -> Result<(), super::session::SessionError> {
        if let Some(bundle) = self.file_message_identity_bundle(manifest)? {
            self.exchange_file_message_identities(channel, vec![], vec![bundle])
                .await?;
        }
        Ok(())
    }

    pub(in crate::node) fn shares_replica(
        &self,
        conversation: &ConversationId,
    ) -> Result<bool, LogError> {
        let me = self.user_id();
        let book = self.channels.lock().expect("channels lock not poisoned");
        if book.state(conversation).is_some() {
            return Ok(book.replica_enabled(*conversation, &me));
        }
        drop(book);
        for scope in self.manifest_scopes(*conversation) {
            let parent = scope?.parent;
            let book = self.channels.lock().expect("channels lock");
            if book.state(&parent).is_some() && !book.replica_enabled(parent, &me) {
                return Ok(false);
            }
        }
        Ok(true)
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
            let existing = match self
                .node
                .log
                .lock()
                .expect("log lock not poisoned")
                .events(&conversation)
            {
                Ok(events) => events,
                Err(error) => return Some(Err(error)),
            };
            if meta.is_member(&me.user_id())
                || meta.owner != event.author.user_id()
                || !meta.is_member(&meta.owner)
                || !existing.is_empty()
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
            if !manual && !self.node.certified_sibling(&self.peer) {
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
            return Some(Err(LogError::AdmissionRejected(
                "uninvited channel staging limit exceeded".into(),
            )));
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

        let approval = {
            let guard = self
                .node
                .privacy
                .state
                .read()
                .expect("privacy lock not poisoned");
            guard
                .as_ref()
                .filter(|state| state.policy.snapshot().invisible)
                .map(|state| -> Result<Option<HashSet<EventId>>, LogError> {
                    let invitation = event.kind == EventKind::MembershipChange
                        && crate::channel::ChannelMeta::is_invitation(&event.ciphertext)
                        && crate::channel::ChannelMeta::decode(&event.ciphertext)
                            .is_some_and(|meta| meta.is_member(&self.node.user_id()));
                    let own_proof = self.node.own_group_proof();
                    let refs: Vec<_> = pending.events.iter().collect();
                    let account =
                        super::channel::ChannelBook::project_membership(conversation, &refs);
                    let account_grant = event.kind == EventKind::ChannelAccountAuthority
                        && (account.authority.is_member(&own_proof.public(), &own_proof)
                            || account
                                .authority
                                .invitation_for_device(&own_proof.public(), &own_proof)
                                .is_some());
                    if !invitation && !account_grant {
                        return Ok(None);
                    }
                    let allowed = self.projection(state, &conversation, &pending.events)?;
                    Ok(allowed.contains(&event.id).then_some(allowed))
                })
                .transpose()
        };
        let approved = match approval {
            Ok(approved) => approved.flatten(),
            Err(error) => return Some(Err(error)),
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
            let still_allowed = match guard
                .as_ref()
                .filter(|state| state.policy.snapshot().invisible)
            {
                Some(state) => match self.projection(state, &conversation, &pending.events) {
                    Ok(allowed) => allowed.contains(&event.id),
                    Err(error) => return Some(Err(error)),
                },
                None => false,
            };
            if !still_allowed {
                self.denied = true;
                return Some(Err(LogError::AdmissionRejected(
                    "channel invitation permission changed".into(),
                )));
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
    ) -> Result<HashSet<EventId>, LogError> {
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
            return Ok(allowed);
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
            return Ok(allowed);
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
            return Ok(allowed);
        }
        // A file conversation inherits its verified manifest's parent scope.
        if self.node.has_file_scope(*conversation)? {
            let scopes = self.node.manifest_scopes(*conversation);
            let mut authors = HashSet::new();
            for scope in scopes {
                let scope = scope?;
                let parent_events: Vec<_> = self
                    .node
                    .log
                    .lock()
                    .expect("log lock not poisoned")
                    .events(&scope.parent)?
                    .into_iter()
                    .collect();
                if scope.parent != *conversation
                    && !self.node.has_file_scope(scope.parent)?
                    && self
                        .projection(state, &scope.parent, &parent_events)?
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
            return Ok(allowed);
        }
        Ok(self.channel_projection(conversation, events, relay, own_peer, Some(state)))
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
        let refs: Vec<_> = events.iter().collect();
        let account_projection =
            super::channel::ChannelBook::project_membership(*conversation, &refs);
        if account_projection.authority.current_event().is_some() {
            let authority = &account_projection.authority;
            let own_proof = self.node.own_group_proof();
            let own_account = self.node.account.public().ed25519_pub;
            let peer_proof = private_state
                .and_then(|state| state.proofs.by_author(&self.peer.ed25519_pub))
                .or_else(|| {
                    if private_state.is_some() {
                        None
                    } else {
                        self.node
                            .roster
                            .lock()
                            .expect("roster lock not poisoned")
                            .historical_announcement(&self.peer)
                            .cloned()
                    }
                })
                .filter(|proof| proof.public() == self.peer && proof.valid_presence());
            // Only authenticated siblings from the existing full-identity pairing
            // registry can fetch account authorization before publishing inheritance.
            let paired = self.node.certified_sibling(&self.peer)
                && peer_proof
                    .as_ref()
                    .and_then(|proof| proof.account_cert.as_ref())
                    .is_some_and(|certificate| certificate.account_ed25519_pub == own_account);
            let me_allowed = authority.account_grant(&own_account).is_some();
            let peer_allowed = peer_proof.as_ref().is_some_and(|proof| {
                authority.is_member(&self.peer, proof)
                    || authority.invitation_for_device(&self.peer, proof).is_some()
            }) || (paired && me_allowed);
            let custody = private_state.is_some_and(|state| {
                state.own.post_office
                    && peer_proof
                        .as_ref()
                        .and_then(|proof| proof.account_id())
                        .is_some_and(|account| {
                            state
                                .policy
                                .snapshot()
                                .allowed_accounts
                                .iter()
                                .any(|allowed| {
                                    allowed.id == account
                                        && allowed.source == PermissionSource::Manual
                                })
                        })
            });
            if (me_allowed || custody) && (peer_allowed || relay) {
                return account_projection.authorized;
            }
            // A removed account may learn its terminal signed decision, but no
            // later metadata, keys or messages. This also repairs offline siblings.
            let mut closing = Vec::new();
            if me_allowed
                && let Some(account) = peer_proof
                    .as_ref()
                    .and_then(|proof| authority.registered_account(&self.peer, proof))
            {
                closing.extend(authority.revocation_events(&account));
            }
            if paired
                || ((peer_allowed || relay)
                    && authority.registered_account(&me, &own_proof).is_some())
            {
                closing.extend(authority.revocation_events(&own_account));
            }
            let graph: HashMap<_, _> = events.iter().map(|event| (event.id, event)).collect();
            let mut ancestors = HashSet::new();
            while let Some(id) = closing.pop() {
                if ancestors.insert(id)
                    && let Some(event) = graph.get(&id)
                {
                    closing.extend_from_slice(&event.parents);
                }
            }
            return account_projection
                .authorized
                .intersection(&ancestors)
                .copied()
                .collect();
        }
        principal::legacy::project(self, *conversation, events, relay, own_peer, private_state)
    }
    fn public_projection_key(
        &self,
        conversation: &ConversationId,
    ) -> Result<Option<super::channel_projection_cache::ProjectionKey>, LogError> {
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
            return Ok(None);
        }
        let privacy_configured = guard.is_some();
        let privacy_generation = self
            .node
            .privacy
            .generation
            .load(std::sync::atomic::Ordering::SeqCst);
        let sharing = self.node.shares_replica(conversation)?;
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
            .fingerprint_and_len(conversation)?;
        Ok(Some(super::channel_projection_cache::ProjectionKey {
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
        }))
    }

    fn public_projection_metadata(
        &self,
        conversation: &ConversationId,
    ) -> Result<Option<super::channel_projection_cache::ProjectionMetadata>, LogError> {
        let Some(key) = self.public_projection_key(conversation)? else {
            return Ok(None);
        };
        let cached = self
            .node
            .channel_projection_cache
            .lock()
            .expect("projection cache lock not poisoned")
            .get(&key);
        if let Some(metadata) = cached {
            if self.public_projection_key(conversation)?.as_ref() == Some(&key) {
                return Ok(Some(metadata));
            }
            return Ok(None);
        }
        // On a miss, detect signed channel ancestry directly in the durable log.
        // ChannelBook may lag behind the append; it is not the public authority.
        let channel = key.established.is_some()
            || self
                .node
                .log
                .lock()
                .expect("log lock not poisoned")
                .events(conversation)?
                .iter()
                .any(|event| event.kind == EventKind::MembershipChange);
        if !channel {
            return Ok(None);
        }
        let visible = self.visible(conversation)?;
        let metadata = super::channel_projection_cache::ProjectionMetadata {
            fingerprint: crate::eventlog::sync::fingerprint(
                visible.iter().map(|event| event.id).collect(),
            ),
            len: visible.len(),
            requires_v5: visible
                .iter()
                .any(|event| event.kind == EventKind::ChannelMetadata),
            requires_v4: visible.iter().any(|event| {
                matches!(
                    event.kind,
                    EventKind::ChannelAccountAuthority | EventKind::ChannelDeviceInheritance
                )
            }),
            requires_v3: visible.iter().any(|event| {
                matches!(
                    event.kind,
                    EventKind::ChannelHistoryPolicy | EventKind::ChannelJoinHistory
                )
            }),
            requires_v2: visible
                .iter()
                .any(|event| event.kind == EventKind::ChannelReplica),
        };
        if self.public_projection_key(conversation)?.as_ref() != Some(&key) {
            return Ok(None);
        }
        self.node
            .channel_projection_cache
            .lock()
            .expect("projection cache lock not poisoned")
            .insert(key, metadata);
        Ok(Some(metadata))
    }

    fn outgoing_projection(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
        payloads: bool,
    ) -> Result<Vec<Event>, LogError> {
        if self.pull_only {
            return Ok(Vec::new());
        }
        let mut events = self.visible_projection(conversation, payloads)?;
        // Recheck each payload page in either direction, including pooled sessions.
        // Preserve the actual possession inventory/fingerprint: hiding retained IDs
        // would make peers push these same chunks back to a metered holder.
        if self
            .node
            .restricts_group_file_replica(conversation, &self.peer)?
        {
            let own = self.node.identity.public().ed25519_pub;
            events.retain(|event| event.author.ed25519_pub() == &own);
        }
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
        if !self.inbound && !self.node.shares_replica(conversation)? {
            // Share-off applies to outbound retries and explicit sends too.
            // Own records can advance only across parents the receiver already
            // has, or other eligible own records in this same ordered batch.
            // Missing third-party ancestry must come from a sharing holder.
            let mut available = have.clone();
            let own = self.node.identity.public().ed25519_pub;
            return Ok(events
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
                .collect());
        }
        Ok(events
            .into_iter()
            .filter(|event| !have.contains(&event.id))
            .collect())
    }

    /// Authorized identities without cloning large message/file ciphertext.
    /// Projection controls retain their exact signed bytes; ordinary Message
    /// payloads are never inspected by the authority reducer (sealed receipts
    /// are the exception and retain their complete bytes).
    pub(in crate::node) fn authorized_event_ids(
        &self,
        conversation: &ConversationId,
    ) -> Result<Vec<EventId>, LogError> {
        Ok(self
            .outgoing_projection(conversation, &HashSet::new(), false)?
            .into_iter()
            .map(|event| event.id)
            .collect())
    }

    pub(in crate::node) fn authorized_event_page(
        &self,
        conversation: &ConversationId,
        wanted: &HashSet<EventId>,
        budget: usize,
    ) -> Result<Vec<Event>, LogError> {
        let ids = self.authorized_event_ids(conversation)?;
        let log = self.node.log.lock().expect("log lock not poisoned");
        let mut used = 0usize;
        let mut events = Vec::new();
        for id in ids.into_iter().filter(|id| wanted.contains(id)) {
            let Some(event) = log.get(&id)? else {
                continue;
            };
            let bytes = event.ciphertext.len().saturating_add(512);
            if used > 0 && used.saturating_add(bytes) > budget {
                break;
            }
            used = used.saturating_add(bytes);
            events.push(event);
        }
        Ok(events)
    }

    fn visible(&self, conversation: &ConversationId) -> Result<Vec<Event>, LogError> {
        self.visible_projection(conversation, true)
    }

    fn visible_projection(
        &self,
        conversation: &ConversationId,
        payloads: bool,
    ) -> Result<Vec<Event>, LogError> {
        if self.inbound && !self.node.shares_replica(conversation)? {
            return Ok(Vec::new());
        }
        let guard = self
            .node
            .privacy
            .state
            .read()
            .expect("privacy lock not poisoned");
        let projected = self
            .node
            .log
            .lock()
            .expect("log lock not poisoned")
            .project_events(conversation, |mut event| {
                if !payloads
                    && event.kind == EventKind::Message
                    && !super::delivery_receipt::valid_sealed_wire(&event.ciphertext)
                {
                    event.ciphertext = Vec::new();
                }
                event
            })?;
        let mut events = crate::eventlog::sync::causal_events(projected);
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
            let allowed =
                self.channel_projection(conversation, &events, false, false, guard.as_ref());
            return Ok(events
                .into_iter()
                .filter(|event| allowed.contains(&event.id))
                .collect());
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
            return Ok(events);
        };
        let allowed = self.projection(state, conversation, &events)?;
        Ok(events
            .into_iter()
            .filter(|e| allowed.contains(&e.id))
            .collect())
    }
}
impl SyncStore for GuardedSyncStore<'_> {
    fn requires_channel_v5(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        if let Some(metadata) = self.public_projection_metadata(conversation)? {
            return Ok(metadata.requires_v5);
        }
        if !self
            .node
            .log
            .lock()
            .expect("log lock")
            .requires_channel_v5(conversation)?
        {
            return Ok(false);
        }
        Ok(self
            .visible(conversation)?
            .iter()
            .any(|event| event.kind == EventKind::ChannelMetadata))
    }
    fn requires_channel_v4(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        if let Some(metadata) = self.public_projection_metadata(conversation)? {
            return Ok(metadata.requires_v4);
        }
        if !self
            .node
            .log
            .lock()
            .expect("log lock not poisoned")
            .requires_channel_v4(conversation)?
        {
            return Ok(false);
        }
        Ok(
            // A private/rejected control must not reveal itself to this principal.
            self.visible(conversation)?.iter().any(|event| {
                matches!(
                    event.kind,
                    EventKind::ChannelAccountAuthority | EventKind::ChannelDeviceInheritance
                )
            }),
        )
    }

    fn requires_channel_v3(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        if let Some(metadata) = self.public_projection_metadata(conversation)? {
            return Ok(metadata.requires_v3);
        }
        if !self
            .node
            .log
            .lock()
            .expect("log lock not poisoned")
            .requires_channel_v3(conversation)?
        {
            return Ok(false);
        }
        Ok(self.visible(conversation)?.iter().any(|event| {
            matches!(
                event.kind,
                EventKind::ChannelHistoryPolicy | EventKind::ChannelJoinHistory
            )
        }))
    }

    fn requires_channel_v2(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        if let Some(metadata) = self.public_projection_metadata(conversation)? {
            return Ok(metadata.requires_v2);
        }
        // Inspect the peer's projection: a hidden event must not affect the
        // compatibility response or reveal that it exists.
        if !self
            .node
            .log
            .lock()
            .expect("log lock not poisoned")
            .requires_channel_v2(conversation)?
        {
            return Ok(false);
        }
        Ok(self
            .visible(conversation)?
            .iter()
            .any(|event| event.kind == EventKind::ChannelReplica))
    }
    fn fingerprint_and_len(
        &self,
        conversation: &ConversationId,
    ) -> Result<([u8; 32], usize), LogError> {
        if let Some(metadata) = self.public_projection_metadata(conversation)? {
            return Ok((metadata.fingerprint, metadata.len));
        }
        let ids = self.event_ids(conversation)?;
        let len = ids.len();
        Ok((crate::eventlog::sync::fingerprint(ids), len))
    }
    fn file_consent(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> Result<crate::eventlog::sync::FileConsentStatus, LogError> {
        use crate::eventlog::sync::FileConsentStatus;
        Ok(
            match self
                .node
                .attachment_consent_for_peer(*file, *manifest, &self.peer)?
            {
                super::attachment_consent::ReceiveDecision::Pending => FileConsentStatus::Pending,
                super::attachment_consent::ReceiveDecision::Accepted => FileConsentStatus::Accepted,
                super::attachment_consent::ReceiveDecision::Declined => FileConsentStatus::Declined,
            },
        )
    }
    fn signed_file_consent(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> Result<Option<crate::eventlog::Event>, LogError> {
        self.node
            .direct_file_consent_control(&self.peer, *file, *manifest)
    }
    fn file_message_identity(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> Result<Option<Event>, LogError> {
        if self.denied
            || self
                .node
                .delivery_suspended
                .load(std::sync::atomic::Ordering::Acquire)
            || !self.node.known_account_allowed(&self.peer)
        {
            return Ok(None);
        }
        let Some(bundle) = self.node.file_message_identity_bundle(*manifest)? else {
            return Ok(None);
        };
        if !super::file_message_identity::bundle_covers(&bundle, *file, *manifest) {
            return Ok(None);
        }
        let Some((source_account, recipient_account)) =
            super::file_message_identity::bundle_accounts(&bundle)
        else {
            return Ok(None);
        };
        let sibling = self.node.certified_sibling(&self.peer);
        if sibling {
            let policy = self.node.device_sync_policy();
            if !policy.history && !policy.attachments {
                return Ok(None);
            }
        } else {
            let Some(peer_account) = self
                .node
                .historical_author(&self.peer.ed25519_pub)
                .filter(|proof| proof.public() == self.peer)
                .and_then(|proof| proof.account_id())
            else {
                return Ok(None);
            };
            if source_account != self.node.account_id() || recipient_account != peer_account {
                return Ok(None);
            }
        }
        let references = super::file_message_identity::bundle_references(&bundle);
        let local_source = source_account == self.node.account_id();
        let host =
            super::conversation::account_conversation_id(&source_account, &recipient_account);
        for (_, alias) in references {
            let row = {
                let files = self
                    .node
                    .received_files
                    .lock()
                    .expect("received files lock");
                match files.entry_in(&host, alias)? {
                    Some(row) => Some(row),
                    None if local_source => files.entry(alias)?,
                    None => None,
                }
            };
            let Some(row) = row else {
                continue;
            };
            if !(super::file_message_identity::bundle_matches_row(&bundle, &row)
                || local_source
                    && super::file_message_identity::bundle_matches_source_row(&bundle, &row))
                || self
                    .node
                    .recipient_expired(&row.plaintext, row.event_id, row.wall_clock)
            {
                continue;
            }
            let delivery = self.node.delivery.lock().expect("delivery lock");
            if delivery.file_erased(row.conversation, row.event_id)
                || delivery.manifest_event_erased(row.event_id)
            {
                continue;
            }
            if !self
                .node
                .account_suppression
                .lock()
                .expect("suppression lock")
                .contains(row.conversation, row.event_id)?
            {
                return Ok(Some(bundle));
            }
        }
        Ok(None)
    }

    fn accept_file_message_identity(&mut self, bundle: Event) -> Result<bool, LogError> {
        if self.denied
            || self.pull_only
            || self
                .node
                .delivery_suspended
                .load(std::sync::atomic::Ordering::Acquire)
            || !self.node.known_account_allowed(&self.peer)
        {
            return Ok(false);
        }
        let Some((source_account, _)) = super::file_message_identity::bundle_accounts(&bundle)
        else {
            return Ok(false);
        };
        if self.node.certified_sibling(&self.peer) {
            let policy = self.node.device_sync_policy();
            if !policy.history && !policy.attachments {
                return Ok(false);
            }
        } else if self
            .node
            .historical_author(&self.peer.ed25519_pub)
            .filter(|proof| proof.public() == self.peer)
            .and_then(|proof| proof.account_id())
            .as_deref()
            != Some(source_account.as_str())
        {
            return Ok(false);
        }
        match self.node.import_file_message_identity(bundle) {
            Ok(()) => Ok(true),
            Err(super::NodeError::Log(error)) if !error.is_peer_rejection() => Err(error),
            Err(_) => Ok(false),
        }
    }
    fn sticker_file_proof(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> Result<Option<Vec<Event>>, LogError> {
        if self.denied || !self.node.known_account_allowed(&self.peer) {
            return Ok(None);
        }
        let authorized = self
            .node
            .delivery
            .lock()
            .expect("delivery lock")
            .cards_for_file(*file)
            .iter()
            .any(|card| {
                card.destinations.iter().any(|destination| {
                    destination.binding.event_id == *manifest
                        && destination.binding.device == self.peer
                })
            });
        if !authorized {
            return Ok(None);
        }
        let host =
            super::conversation::dm_conversation_id(&self.node.identity.public(), &self.peer);
        if !self
            .visible_projection(&host, false)?
            .iter()
            .any(|event| event.id == *manifest && event.kind == EventKind::FileManifest)
        {
            return Ok(None);
        }
        let Some(headers) = self.node.sticker_chunk_proof(*file)? else {
            return Ok(None);
        };
        let visible: HashSet<_> = self
            .visible_projection(file, false)?
            .into_iter()
            .map(|event| event.id)
            .collect();
        Ok(headers
            .iter()
            .all(|event| visible.contains(&event.id))
            .then_some(headers))
    }
    fn sticker_file_consent(
        &mut self,
        file: &ConversationId,
        manifest: &EventId,
        headers: Vec<Event>,
    ) -> Result<Option<Event>, LogError> {
        if self.denied || self.pull_only {
            return Ok(None);
        }
        if let Some(chunks) = self
            .node
            .reconstruct_sticker_proof(*file, *manifest, &self.peer, headers)?
        {
            for chunk in chunks {
                self.ingest(chunk)?;
            }
            self.node.persist_media_if_complete(*file);
            self.node.note_file_change();
        }
        self.signed_file_consent(file, manifest)
    }
    fn durable_have(&self, conversation: &ConversationId, id: &EventId) -> Result<bool, LogError> {
        if !self
            .visible_projection(conversation, false)?
            .iter()
            .any(|e| e.id == *id)
        {
            return Ok(!self.denied
                && self
                    .node
                    .historical_file_completion(*conversation, *id, &self.peer)?);
        }
        self.node
            .log
            .lock()
            .expect("log lock not poisoned")
            .sync()?;
        Ok(true)
    }
    fn admission_denied(&self) -> bool {
        self.denied
    }
    fn event_ids(&self, conversation: &ConversationId) -> Result<Vec<EventId>, LogError> {
        let mut ids: Vec<_> = self.visible(conversation)?.iter().map(|e| e.id).collect();
        if let Some(pending) = self.pending_channels.get(conversation) {
            ids.extend(pending.events.iter().map(|event| event.id));
        }
        Ok(ids)
    }
    fn events_excluding(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
    ) -> Result<Vec<Event>, LogError> {
        self.outgoing_projection(conversation, have, true)
    }
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        let node = self.node;
        let _membership = matches!(
            event.kind,
            EventKind::MembershipChange
                | EventKind::ChannelAccountAuthority
                | EventKind::ChannelDeviceInheritance
                | EventKind::ChannelMetadata
                | EventKind::ChannelAccept
                | EventKind::ChannelLeave
                | EventKind::ChannelOwnerTransfer
                | EventKind::ChannelDissolve
        )
        .then(|| {
            node.channel_membership_commit
                .lock()
                .expect("channel membership commit lock")
        });
        if let Some(result) = self.stage_uninvited_channel(&event) {
            return result;
        }
        let known_file = {
            self.node
                .files
                .lock()
                .expect("files lock not poisoned")
                .contains_file(event.conversation_id)?
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
                    .events(&event.conversation_id)?
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
                return Err(LogError::AdmissionRejected(
                    "unknown attachment conversation".into(),
                ));
            }
        }
        // A legacy peer can skip the optional consent query and push chunks through
        // ordinary sync. Enforce the same receive decision at the durable log gate.
        if event.kind == EventKind::Message
            && known_file
            && !self.node.attachment_chunk_author_allowed(
                event.conversation_id,
                event.author.ed25519_pub(),
            )?
        {
            self.denied = true;
            return Err(LogError::AdmissionRejected(
                "attachment awaiting recipient approval".into(),
            ));
        }
        if event.kind == EventKind::Message && known_file {
            let manifest = self
                .node
                .files
                .lock()
                .expect("files lock not poisoned")
                .manifest(&event.conversation_id)?;
            if let Some(manifest) = manifest.filter(super::media_store::automatic_inline_image) {
                let first_chunk = self
                    .node
                    .log
                    .lock()
                    .expect("log lock not poisoned")
                    .events(&event.conversation_id)?
                    .is_empty();
                if first_chunk && (event.seq != 1 || !event.parents.is_empty()) {
                    self.denied = true;
                    return Err(LogError::AdmissionRejected(
                        "automatic image must start at chunk one".into(),
                    ));
                }
                if event.seq == 1 {
                    let valid = crate::file::open_chunk_for(&manifest, 0, &event.ciphertext)
                        .ok()
                        .is_some_and(|bytes| {
                            super::media_store::raster_signature_matches(manifest.mime(), &bytes)
                        });
                    if !valid {
                        self.denied = true;
                        return Err(LogError::AdmissionRejected(
                            "automatic image has invalid image signature".into(),
                        ));
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
                .events(&event.conversation_id)?
                .into_iter()
                .collect();
            events.push(event.clone());
            events.sort_by_key(|e| (e.lamport, e.id));
            if !self
                .projection(state, &event.conversation_id, &events)?
                .contains(&event.id)
            {
                self.denied = true;
                return Err(LogError::AdmissionRejected("event admission denied".into()));
            }
        }
        let mut log = self.node.log.lock().expect("log lock not poisoned");
        if event.kind == EventKind::Message
            && known_file
            && log
                .events(&event.conversation_id)?
                .iter()
                .any(|chunk| chunk.author != event.author)
        {
            self.denied = true;
            return Err(LogError::AdmissionRejected(
                "attachment chunk stream already belongs to another sender".into(),
            ));
        }
        if matches!(event.kind, EventKind::Message | EventKind::FileManifest) {
            log.append_durable(event)
        } else {
            log.append(event)
        }
    }
}

#[cfg(test)]
mod causal_export_tests {
    #[tokio::test]
    async fn rejected_account_authority_never_exposes_a_v4_requirement_to_legacy_members() {
        use crate::channel::ChannelMeta;
        use crate::discovery::Announce;
        use crate::identity::{account::Account, device::DeviceIdentity};
        let dir = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let peer_account = Account::generate();
        let proof = Announce::new_with_account(&peer, &peer_account, "Peer", 9);
        let (node, _, _) = super::super::test_support::node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let channel = ConversationId::new([173; 32]);
        let initial = Event::new(
            &node.identity,
            channel,
            1,
            vec![],
            1,
            0,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "Legacy group".into(),
                members: vec![node.identity.public(), peer.public()],
                epoch: 0,
                owner: node.user_id(),
            }
            .encode(),
        );
        // A real owner signature is insufficient: the account authority body
        // still needs its independent account-root commitment and strict reducer.
        let rejected = Event::new(
            &node.identity,
            channel,
            2,
            vec![initial.id],
            2,
            0,
            EventKind::ChannelAccountAuthority,
            b"invalid account authority".to_vec(),
        );
        {
            let mut log = node.log.lock().unwrap();
            log.append_durable(initial.clone()).unwrap();
            log.append_durable(rejected.clone()).unwrap();
            assert!(log.requires_channel_v4(&channel).unwrap());
        }
        for private in [false, true] {
            if private {
                node.configure_privacy(
                    dir.path(),
                    "pw",
                    &node.own_group_proof(),
                    std::sync::Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
                )
                .unwrap();
                node.privacy
                    .state
                    .write()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .proofs
                    .record(&proof)
                    .unwrap();
                node.set_allowed(&peer_account.account_id(), true)
                    .await
                    .unwrap();
            }
            let guarded = node.sync_store(&peer.public());
            let store = guarded.lock().unwrap();
            assert_eq!(store.event_ids(&channel).unwrap(), vec![initial.id]);
            assert!(!store.requires_channel_v4(&channel).unwrap());
            assert!(
                !store.requires_channel_v4(&channel).unwrap(),
                "cached projection must stay principal-scoped"
            );
            assert!(!store.event_ids(&channel).unwrap().contains(&rejected.id));
        }
    }
    #[tokio::test]
    async fn authorized_metadata_ids_and_bounded_pages_match_full_projection() {
        use crate::discovery::Announce;
        use crate::identity::{account::Account, device::DeviceIdentity};
        let dir = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
        let (node, _, _) = super::super::test_support::node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let conversation =
            super::super::conversation::dm_conversation_id(&node.identity.public(), &peer.public());
        let parent = Event::new(
            &node.identity,
            conversation,
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![8; 40_000],
        );
        let child = Event::new(
            &node.identity,
            conversation,
            2,
            vec![parent.id],
            2,
            0,
            EventKind::Message,
            vec![9; 40_000],
        );
        node.log
            .lock()
            .unwrap()
            .append_durable(parent.clone())
            .unwrap();
        node.log
            .lock()
            .unwrap()
            .append_durable(child.clone())
            .unwrap();
        let guarded = node.sync_store(&peer.public());
        let store = guarded.lock().unwrap();
        assert_eq!(
            store.authorized_event_ids(&conversation).unwrap(),
            store.event_ids(&conversation).unwrap()
        );
        let wanted = HashSet::from([parent.id, child.id]);
        assert_eq!(
            store
                .authorized_event_page(&conversation, &wanted, 50_000)
                .unwrap(),
            vec![parent]
        );
        assert_eq!(
            store
                .authorized_event_page(&conversation, &HashSet::from([child.id]), 50_000)
                .unwrap(),
            vec![child]
        );
    }

    use super::*;
    use crate::eventlog::sync::{build_request, handle_request_bounded, handle_response};

    #[tokio::test]
    async fn loopback_guarded_sync_crosses_frame_cap_with_inverted_clocks() {
        use crate::transport::SecureChannel;
        use tokio::net::{TcpListener, TcpStream};
        let dir = tempfile::tempdir().unwrap();
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (channel, _) = tokio::sync::mpsc::unbounded_channel();
        let (file, _) = tokio::sync::mpsc::unbounded_channel();
        let node = Node::open(
            crate::identity::device::DeviceIdentity::generate(),
            std::sync::Arc::new(std::sync::Mutex::new(crate::discovery::Roster::default())),
            dm,
            channel,
            file,
            &dir.path().join("events.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        let conversation = ConversationId::new([52; 32]);
        // Each event fits, but their combined payload exceeds one Noise frame.
        let parent = Event::new(
            &node.identity,
            conversation,
            1,
            vec![],
            100,
            0,
            EventKind::Message,
            vec![1; 40 * 1024],
        );
        let child = Event::new(
            &node.identity,
            conversation,
            2,
            vec![parent.id],
            1,
            0,
            EventKind::Message,
            vec![2; 40 * 1024],
        );
        {
            let mut log = node.log.lock().unwrap();
            log.append_durable(parent.clone()).unwrap();
            log.append_durable(child.clone()).unwrap();
        }
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut channel = SecureChannel::accept(stream, &node.identity).await.unwrap();
            let store = node.sync_store(channel.peer_identity());
            while !matches!(
                super::super::session::serve_one(&mut channel, &store)
                    .await
                    .unwrap(),
                super::super::session::Served::Closed
            ) {}
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let identity = crate::identity::device::DeviceIdentity::generate();
            let stream = TcpStream::connect(address).await.unwrap();
            let mut channel = SecureChannel::connect(stream, &identity, None)
                .await
                .unwrap();
            let target = std::sync::Mutex::new(EventLog::default());
            let report = super::super::session::request_round(&mut channel, &target, conversation)
                .await
                .unwrap();
            assert_eq!(report.applied, 2);
            assert!(report.rejected.is_empty());
            assert!(target.lock().unwrap().has(&parent.id));
            assert!(target.lock().unwrap().has(&child.id));
            drop(channel);
            server.await.unwrap();
        })
        .await
        .expect("bounded sync must converge across inverted-clock response frames");
    }

    #[tokio::test]
    async fn guarded_bounded_sync_preserves_inverted_clock_ancestry() {
        let dir = tempfile::tempdir().unwrap();
        let author = crate::identity::device::DeviceIdentity::generate();
        let peer = crate::identity::device::DeviceIdentity::generate();
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (channel, _) = tokio::sync::mpsc::unbounded_channel();
        let (file, _) = tokio::sync::mpsc::unbounded_channel();
        let node = Node::open(
            author,
            std::sync::Arc::new(std::sync::Mutex::new(crate::discovery::Roster::default())),
            dm,
            channel,
            file,
            &dir.path().join("events.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        let conversation = ConversationId::new([51; 32]);
        let parent = Event::new(
            &node.identity,
            conversation,
            1,
            vec![],
            100,
            0,
            EventKind::Message,
            b"parent".to_vec(),
        );
        let child = Event::new(
            &node.identity,
            conversation,
            2,
            vec![parent.id],
            1,
            0,
            EventKind::Message,
            b"child".to_vec(),
        );
        {
            let mut log = node.log.lock().unwrap();
            log.append_durable(parent.clone()).unwrap();
            log.append_durable(child.clone()).unwrap();
        }
        let guarded = node.sync_store(&peer.public());
        let mut target = EventLog::default();
        for expected in [parent.id, child.id] {
            let response = handle_request_bounded(
                &*guarded.lock().unwrap(),
                &build_request(&target, conversation).unwrap(),
                1,
            )
            .unwrap();
            assert_eq!(response.events.len(), 1);
            assert_eq!(response.events[0].id, expected);
            let (report, _) = handle_response(&mut target, &response).unwrap();
            assert_eq!(report.applied, 1);
            assert!(report.rejected.is_empty());
        }
    }
}
