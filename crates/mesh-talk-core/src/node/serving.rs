//! Outbound delivery (direct + post-office replication) and the inbound serve loop. Split out of node.rs (one `impl Node` block per domain).

use super::*;
use crate::discovery::roster::PeerRecord;
use crate::eventlog::event::{Author, ConversationId, Event, EventId, EventKind};
use crate::node::conversation::account_conversation_id;
use crate::node::session::{
    Served, SessionError, request_round_paged as request_round, serve_one_paged as serve_one,
    serve_wire_bytes_paged as serve_wire_bytes,
};
use crate::transport::SecureChannel;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

/// Per-connection round ceiling for an inbound serve loop (matches the requester-side
/// `MAX_SYNC_ROUNDS`); a real reconciliation converges far below this.
const MAX_SERVE_ROUNDS: usize = 10_000;
/// Drop an inbound connection that sends nothing for this long.
const SERVE_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Leave one shared TCP slot available to outbound progress. Excess inbound
/// streams wait in the OS queue; the Node's shared pool also counts handshakes.
pub(super) const MAX_CONCURRENT_CONNS: usize = super::session_pool::MAX_DIRECTION_CONNECTIONS;

// A received event fits one transport frame (<64 KiB). Limiting a processing
// pass to 64 messages bounds wrapped+decoded failed plaintext to about 8 MiB.
const CHANNEL_PLAINTEXT_BATCH: usize = 64;

#[derive(Default)]
pub(in crate::node) struct PendingChannelPlaintext {
    messages: Vec<super::channel::ReceivedChannelMessage>,
    pub(in crate::node) retry_at: Option<std::time::Instant>,
    delay_ms: u64,
    batch_id: Option<String>,
    batch_size: usize,
    // Contains metadata only, never decrypted text or a wrapped body.
    terminal: Option<super::channel::ReceivedChannelMessage>,
}

const POST_OFFICE_DRAIN_INTERVAL: Duration = Duration::from_secs(3);
const MAX_DRAIN_DUE_ENTRIES: usize = 256;
type DrainKey = super::session_pool::PeerEndpointKey;

#[derive(Default)]
struct PostOfficeDrainSchedule {
    primary: Option<DrainKey>,
    due: std::collections::VecDeque<(DrainKey, tokio::time::Instant)>,
    alternative_cursor: usize,
}

impl PostOfficeDrainSchedule {
    fn due_at(&self, key: &DrainKey, now: tokio::time::Instant) -> tokio::time::Instant {
        self.due
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map_or(now, |(_, at)| *at)
    }

    fn select(
        &mut self,
        candidates: &[(DrainKey, PeerRecord)],
        active: &[DrainKey],
        now: tokio::time::Instant,
    ) -> Option<usize> {
        self.due.retain(|(_, at)| *at > now);
        // Reserve one bounded due entry for every active exchange. Never evict
        // an unexpired deadline merely because many other relays finish quickly.
        if active.len() >= 2 || self.due.len() + active.len() >= MAX_DRAIN_DUE_ENTRIES {
            return None;
        }
        let ready = |key: &DrainKey| !active.contains(key) && self.due_at(key, now) <= now;
        if let Some(primary) = self.primary.as_ref() {
            if let Some(index) = candidates
                .iter()
                .position(|(key, _)| key == primary && ready(key))
            {
                return Some(index);
            }
            // Reserve the primary's idle slot instead of filling both slots
            // with slower alternatives before its next scheduled pull.
            if active.iter().any(|key| key != primary) {
                return None;
            }
        } else if active.is_empty() {
            return candidates.iter().position(|(key, _)| ready(key));
        }
        for offset in 0..candidates.len() {
            let index = self.alternative_cursor.wrapping_add(offset) % candidates.len();
            let key = &candidates[index].0;
            if self.primary.as_ref() != Some(key) && ready(key) {
                self.alternative_cursor = index.wrapping_add(1);
                return Some(index);
            }
        }
        None
    }

    fn completed(&mut self, key: DrainKey, result: Option<bool>, now: tokio::time::Instant) {
        if result == Some(true) && self.primary.is_none() {
            self.primary = Some(key.clone());
        } else if result != Some(true) && self.primary.as_ref() == Some(&key) {
            self.primary = None;
        }
        self.due.retain(|(candidate, _)| candidate != &key);
        if self.due.len() >= MAX_DRAIN_DUE_ENTRIES {
            self.due.pop_front();
        }
        self.due.push_back((key, now + POST_OFFICE_DRAIN_INTERVAL));
    }

    fn next_wake(
        &self,
        candidates: &[(DrainKey, PeerRecord)],
        active: &[DrainKey],
        now: tokio::time::Instant,
    ) -> tokio::time::Instant {
        candidates
            .iter()
            .filter(|(key, _)| {
                !active.contains(key)
                    && active.len() < 2
                    && (self.primary.as_ref() == Some(key)
                        || self
                            .primary
                            .as_ref()
                            .is_none_or(|primary| !active.iter().any(|key| key != primary)))
            })
            .map(|(key, _)| {
                let due = self.due_at(key, now);
                if due <= now && self.due.len() + active.len() >= MAX_DRAIN_DUE_ENTRIES {
                    self.due
                        .iter()
                        .map(|(_, at)| *at)
                        .min()
                        .unwrap_or(now + POST_OFFICE_DRAIN_INTERVAL)
                } else {
                    due
                }
            })
            .min()
            .unwrap_or(now + POST_OFFICE_DRAIN_INTERVAL)
            .min(now + POST_OFFICE_DRAIN_INTERVAL)
    }
}

impl Node {
    /// Dial `peer` directly and run one sync round for `conv`. Best-effort: the
    /// peer may be offline, in which case the dial fails.
    pub(in crate::node) async fn deliver_direct(
        &self,
        peer: &PeerRecord,
        conv: ConversationId,
    ) -> Result<(), SessionError> {
        if peer.post_office {
            return Err(SessionError::UnexpectedMessage);
        }
        for fresh in [false, true] {
            let (mut channel, reused) = if fresh {
                self.privacy_dial(peer.addr, &peer.public)
                    .await
                    .map(|channel| (channel, false))
            } else {
                self.privacy_sync_dial_with_reuse(peer.addr, &peer.public)
                    .await
            }
            .map_err(SessionError::Transport)?;
            let store = self.sync_store(channel.peer_identity());
            match request_round(&mut channel, &store, conv).await {
                Ok(_) => {
                    if conv
                        == super::conversation::dm_conversation_id(
                            &self.identity.public(),
                            &peer.public,
                        )
                        && !self.certified_sibling(&peer.public)
                        && self.file_identity_capability_due(peer.addr, &peer.public)
                    {
                        let requests = self.pending_file_message_identities(&peer.public);
                        let offers = self.file_message_identity_controls_for(&peer.public, true)?;
                        if (!requests.is_empty() || !offers.is_empty())
                            && self
                                .exchange_file_message_identities(&mut channel, requests, offers)
                                .await
                                .is_err()
                        {
                            // An optional metadata query may close an old endpoint;
                            // the preceding ordinary reconciliation is already durable.
                            return Ok(());
                        }
                    }
                    self.recycle_sync_channel(channel);
                    return Ok(());
                }
                Err(error) if reused && super::session_pool::retryable_sync_error(&error) => {}
                Err(error) => return Err(error),
            }
            // A peer can close immediately after the idle liveness check.
            // Signed idempotent reconciliation safely retries a fresh session.
        }
        Err(SessionError::UnexpectedMessage)
    }

    /// Replicate `conv` to the elected post office, if one is known. Returns
    /// `Ok(true)` if a post office accepted a round, `Ok(false)` if no post office
    /// is known.
    pub(in crate::node) async fn replicate_to_post_office(
        &self,
        conv: ConversationId,
    ) -> Result<bool, SessionError> {
        let me = self.identity.public();
        let targets = self
            .historical_dm_peers()
            .into_iter()
            .map(|proof| proof.public())
            .filter(|recipient| {
                conv == crate::node::conversation::dm_conversation_id(&me, recipient)
                    || conv
                        == crate::node::delivery_receipt::delivery_conversation_id(&me, recipient)
            })
            .collect();
        self.replicate_to_post_office_targets(conv, targets).await
    }

    pub(in crate::node) async fn replicate_to_post_office_targets(
        &self,
        conv: ConversationId,
        targets: Vec<crate::identity::device::PublicIdentity>,
    ) -> Result<bool, SessionError> {
        self.replicate_to_post_office_targets_confirming(conv, targets, None)
            .await
    }

    pub(in crate::node) async fn replicate_to_post_office_targets_confirming(
        &self,
        conv: ConversationId,
        targets: Vec<crate::identity::device::PublicIdentity>,
        confirm: Option<EventId>,
    ) -> Result<bool, SessionError> {
        if targets.is_empty() {
            return Ok(false);
        }
        let mut uploaded = false;
        for recipient in &targets {
            let candidates = {
                let roster = self.roster.lock().expect("roster mutex not poisoned");
                self.post_office_candidates_for(&roster, recipient)
            };
            let mut complete = false;
            for po in candidates {
                if !self.relay_send_allowed(&po.public) {
                    continue;
                }
                let Ok(mut channel) = self.privacy_sync_dial(po.addr, &po.public).await else {
                    continue;
                };
                if !self.relay_send_allowed(&po.public) {
                    continue;
                }
                let Ok(events) = self
                    .opaque_relay_round(&mut channel, recipient, Some(conv))
                    .await
                else {
                    continue;
                };
                // Sync completion is not durable custody. Probe the exact
                // recipient-encrypted wrapper before publishing a relay copy.
                if let Some(id) = confirm {
                    let mut mailboxes = vec![
                        crate::postoffice::opaque::RelayMailbox::new(
                            &self.identity,
                            recipient,
                            &po.public,
                        )
                        .map_err(|error| SessionError::Serialization(error.to_string()))?,
                    ];
                    if self.bridge_directory_allowed() {
                        mailboxes.push(
                            crate::postoffice::opaque::RelayMailbox::federated(
                                &self.identity,
                                recipient,
                            )
                            .map_err(|error| SessionError::Serialization(error.to_string()))?,
                        );
                    }
                    let mut held = false;
                    for (_, outer) in events.iter().filter(|(inner, _)| *inner == id) {
                        for mailbox in &mailboxes {
                            if super::session::request_durable_have(
                                &mut channel,
                                mailbox.id(),
                                *outer,
                            )
                            .await?
                            {
                                held = true;
                                break;
                            }
                        }
                        if held {
                            break;
                        }
                    }
                    if held {
                        self.publish_channel_custody(conv, id, &po.public, true, false, true)
                            .map_err(|error| SessionError::Serialization(error.to_string()))?;
                        self.channel_custody
                            .lock()
                            .expect("channel custody lock")
                            .confirm(id, &format!("relay:{}", po.public.user_id()))
                            .map_err(|error| SessionError::Serialization(error.to_string()))?;
                        self.delivery_status_notify.notify_waiters();
                    }
                }
                self.recycle_sync_channel(channel);
                uploaded |= !events.is_empty();
                complete = true;
                break;
            }
            if !complete {
                return Ok(false);
            }
        }
        Ok(uploaded)
    }

    /// Accept inbound connections and maintain deferred profile compactions.
    /// Maintenance runs on the blocking pool and stops when this future is dropped;
    /// CLI and SDK hosts get the same log maintenance as the desktop runtime.
    pub async fn run_accept_loop(self: Arc<Self>, listener: TcpListener) {
        let changes = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .changes();
        tokio::select! {
            _ = Arc::clone(&self).run_profile_compaction_loop() => {}
            _ = Arc::clone(&self).run_delivery_loop() => {}
            _ = Arc::clone(&self).run_channel_control_retry_loop() => {}
            _ = Arc::clone(&self).run_channel_recovery_loop(changes) => {}
            _ = Arc::clone(&self).run_account_history_loop() => {}
            _ = Arc::clone(&self).run_account_wake_loop() => {}
            _ = Arc::clone(&self).run_expiry_loop() => {}
            _ = self.accept_connections(listener) => {}
        }
    }

    async fn run_expiry_loop(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let Some(work) = self.runtime_work.admit() else {
                return;
            };
            let node = Arc::clone(&self);
            match work
                .spawn_blocking(move || {
                    #[cfg(test)]
                    if let Some(hook) = node.expiry_cleanup_hook.lock().unwrap().take() {
                        hook();
                    }
                    node.prune_expired_received()
                })
                .await
                .map(|value| value.into_inner())
            {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => log::warn!("message expiry cleanup failed: {error}"),
                Err(error) => log::warn!("message expiry task failed: {error}"),
            }
        }
    }

    async fn run_channel_control_retry_loop(self: Arc<Self>) {
        loop {
            self.retry_channel_controls().await;
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    }

    async fn accept_connections(self: Arc<Self>, listener: TcpListener) {
        let conns = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNS));
        // Dropping this loop aborts children; their runtime work permits remain
        // held until Tokio actually destroys each connection future.
        let mut connections = tokio::task::JoinSet::new();
        loop {
            while connections.try_join_next().is_some() {}
            // Reserve a connection slot BEFORE accepting, so we never serve more than the cap;
            // excess inbound connections wait in the OS accept queue until a slot frees.
            let permit = match Arc::clone(&conns).acquire_owned().await {
                Ok(p) => p,
                Err(_) => return, // semaphore closed — shouldn't happen, stop cleanly
            };
            let tcp_permits = match self.sessions.inbound().await {
                Ok(permits) => permits,
                Err(_) => {
                    drop(permit);
                    continue;
                }
            };
            // Only the (fast) TCP accept runs on the loop; the Noise handshake runs in the
            // spawned task (bounded by HANDSHAKE_TIMEOUT), so a peer that connects then stalls
            // mid-handshake can't park the loop and block all other inbound connections.
            let stream = match listener.accept().await {
                Ok((stream, _addr)) => stream,
                Err(_) => {
                    // A listener-level error (e.g. fd exhaustion) shouldn't stop the loop;
                    // back off briefly so a persistent error can't become a busy-spin.
                    drop(permit);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let node = Arc::clone(&self);
            let Some(work) = self.runtime_work.admit() else {
                return;
            };
            connections.spawn(work.track(async move {
                let _permit = permit; // held for the connection's lifetime, freed on drop
                #[cfg(test)]
                if let Some(hook) = node.accepted_hook.lock().unwrap().take() {
                    let _ = hook.send(());
                }
                if let Ok(channel) = node.privacy_accept_with_permits(stream, tcp_permits).await {
                    node.serve_connection(channel).await;
                }
            }));
        }
    }

    /// Serve one authenticated inbound connection: handle sync rounds and surface
    /// any newly-received DMs, until the peer disconnects. The first frame is peeked:
    /// a device-pairing request gets the linking handler; anything else is a sync wire
    /// and is served normally (then the loop continues).
    pub async fn serve_connection(self: &Arc<Self>, mut channel: SecureChannel<TcpStream>) {
        if channel.pairing_only() {
            // A retired account proof can recover an interrupted authorized link,
            // but can never enter sync, account history, attachments or call IPC.
            if let Ok(Ok(first)) = tokio::time::timeout(SERVE_IDLE_TIMEOUT, channel.recv()).await {
                if let Some((request, history)) = PairingRequest::decode_access(&first) {
                    self.serve_pairing_access(&mut channel, request, history)
                        .await;
                } else if let Some((request, history, metadata)) =
                    PairingRequest::decode_negotiated(&first)
                {
                    self.serve_pairing_negotiated(&mut channel, request, history, metadata)
                        .await;
                }
            }
            return;
        }
        // A relay may initiate a sync round too. The account preference applies to
        // inbound relay sessions as well as our own upload/drain dials.
        let is_relay = channel.peer_announcement().is_some_and(|a| a.post_office)
            || self
                .roster
                .lock()
                .expect("roster mutex not poisoned")
                .get(&channel.peer_identity().user_id())
                .is_some_and(|peer| peer.public == *channel.peer_identity() && peer.post_office);
        if is_relay && !self.relay_allowed(channel.peer_identity()) {
            return;
        }
        // Relay traffic is initiated by this node through opaque pairwise
        // mailboxes. An inbound relay must never be able to request the local
        // signed event log, whose headers and channel controls are readable.
        if is_relay {
            return;
        }
        let store = self.serve_sync_store(channel.peer_identity());
        // Bound the connection so an authenticated peer can't pin a task forever: an idle
        // timeout on every recv + a per-connection round ceiling (mirrors the relay).
        let first = match tokio::time::timeout(SERVE_IDLE_TIMEOUT, channel.recv()).await {
            Ok(Ok(b)) => b,
            _ => return, // peer error or idle past the timeout
        };
        if is_relay && !self.relay_allowed(channel.peer_identity()) {
            return;
        }
        if let Some(request) = super::channel_account_bootstrap::AccountGroupRequest::decode(&first)
        {
            self.serve_account_group_request(&mut channel, request)
                .await;
            return;
        }
        if let Some(request) = super::account_access::AccessRequest::decode(&first) {
            self.serve_account_access(&mut channel, request).await;
            return;
        }
        if let Some((request, history)) = PairingRequest::decode_access(&first) {
            if self.account_allowed(channel.peer_identity(), channel.peer_announcement()) {
                self.serve_pairing_access(&mut channel, request, history)
                    .await;
            }
            return;
        }
        if let Some((req, history, metadata)) = PairingRequest::decode_negotiated(&first) {
            if !self.account_allowed(channel.peer_identity(), channel.peer_announcement()) {
                return;
            }
            self.serve_pairing_negotiated(&mut channel, req, history, metadata)
                .await;
            return;
        }
        if let Some((
            super::account_sync_wire::AccountSyncVersion::V2,
            super::account_sync_wire::AccountHistoryFrame::SessionRequest {
                request_id,
                request,
            },
        )) = super::account_sync_wire::AccountHistoryFrame::decode_versioned(&first)
        {
            self.serve_account_session(channel, request_id, request)
                .await;
            return;
        }
        if let Some((
            version,
            super::account_sync_wire::AccountHistoryFrame::Wake {
                account_id,
                epoch,
                revision,
            },
        )) = super::account_sync_wire::AccountHistoryFrame::decode_versioned(&first)
        {
            self.serve_account_wake(&mut channel, version, &account_id, epoch, revision)
                .await;
            return;
        }
        if let Some((
            super::account_sync_wire::AccountSyncVersion::V2,
            super::account_sync_wire::AccountHistoryFrame::DocumentsRequest { body },
        )) = super::account_sync_wire::AccountHistoryFrame::decode_versioned(&first)
        {
            if let Some(request) = super::account_documents::wire::DocumentRequest::decode(&body) {
                self.serve_account_documents(&mut channel, request).await;
            }
            return;
        }
        if let Some((
            super::account_sync_wire::AccountSyncVersion::V2,
            super::account_sync_wire::AccountHistoryFrame::FavoritesRequest {
                version: 1,
                account_id,
                after,
                known_digest,
                ..
            },
        )) = super::account_sync_wire::AccountHistoryFrame::decode_versioned(&first)
        {
            self.serve_message_favorites(&mut channel, &account_id, after, known_digest)
                .await;
            return;
        }
        if let Some((
            super::account_sync_wire::AccountSyncVersion::V2,
            super::account_sync_wire::AccountHistoryFrame::FavoriteAttachmentRequest {
                version: 1,
                account_id,
                file_conv,
            },
        )) = super::account_sync_wire::AccountHistoryFrame::decode_versioned(&first)
        {
            self.serve_favorite_attachment(
                &mut channel,
                &account_id,
                crate::eventlog::event::ConversationId::new(file_conv),
            )
            .await;
            return;
        }
        if let Some((
            version,
            super::account_sync_wire::AccountHistoryFrame::Request {
                account_id,
                cursor,
                known_digest,
                wanted,
                ..
            },
        )) = super::account_sync_wire::AccountHistoryFrame::decode_versioned(&first)
        {
            self.serve_account_history(
                &mut channel,
                version,
                &account_id,
                cursor,
                known_digest,
                wanted,
            )
            .await;
            return;
        }
        if let Some((
            super::account_sync_wire::AccountSyncVersion::V2,
            super::account_sync_wire::AccountHistoryFrame::FileMessageIdentitiesRequest {
                account_id,
                requests,
            },
        )) = super::account_sync_wire::AccountHistoryFrame::decode_versioned(&first)
        {
            self.serve_account_file_message_identities(&mut channel, &account_id, requests)
                .await;
            return;
        }
        if let Some((
            super::account_sync_wire::AccountSyncVersion::V2,
            super::account_sync_wire::AccountHistoryFrame::AttachmentRequestChecked {
                account_id,
                file_conv,
            },
        )) = super::account_sync_wire::AccountHistoryFrame::decode_versioned(&first)
        {
            self.serve_account_attachment_checked(
                &mut channel,
                &account_id,
                crate::eventlog::event::ConversationId::new(file_conv),
            )
            .await;
            return;
        }
        if let Some((
            version,
            super::account_sync_wire::AccountHistoryFrame::AttachmentRequest {
                account_id,
                file_conv,
                ..
            },
        )) = super::account_sync_wire::AccountHistoryFrame::decode_versioned(&first)
        {
            self.serve_account_attachment(
                &mut channel,
                version,
                &account_id,
                crate::eventlog::event::ConversationId::new(file_conv),
            )
            .await;
            return;
        }
        // A call signal (SDP/bye): surface it live (bound to the authenticated peer) and
        // close — it is ephemeral and never enters the sync/event-log path.
        if let Some(signal) = crate::node::call::CallSignal::decode(&first) {
            let _operation = self.privacy.gate.read().await;
            if !channel.io_admitted() {
                return;
            }
            if !self.account_allowed(channel.peer_identity(), channel.peer_announcement()) {
                return;
            }
            self.serve_call_signal(&channel, signal);
            return;
        }
        // The first frame may be a Request, which makes serve_wire_bytes await the peer's
        // streamed have-chunks — so it needs the same idle timeout as the loop, or a peer that
        // sends one Request then stalls would pin this task (and its connection permit) forever.
        match tokio::time::timeout(
            SERVE_IDLE_TIMEOUT,
            serve_wire_bytes(&mut channel, &store, &first),
        )
        .await
        {
            Ok(Ok(Served::Handled(conv))) => {
                self.emit_new_messages(conv);
                self.process_channel(conv);
                self.process_file_events(conv);
                self.process_profile_events(conv);
            }
            _ => return,
        }
        for _ in 0..MAX_SERVE_ROUNDS {
            if is_relay && !self.relay_allowed(channel.peer_identity()) {
                break;
            }
            match tokio::time::timeout(SERVE_IDLE_TIMEOUT, serve_one(&mut channel, &store)).await {
                Ok(Ok(Served::Handled(conv))) => {
                    self.emit_new_messages(conv);
                    self.process_channel(conv);
                    self.process_file_events(conv);
                    self.process_profile_events(conv);
                }
                _ => break, // peer closed, error, or idle past the timeout
            }
        }
    }

    /// Open and surface any new `FileManifest` events in `conv`. Channel manifests
    /// open with the channel group key; DM manifests with the DM sealed-box (the
    /// author's X25519 from the roster). A no-op for conversations with no new
    /// manifest events. Own manifests are skipped (the sender already knows).
    pub(in crate::node) fn process_file_events(&self, conv: ConversationId) {
        self.process_file_events_reception(conv, "live");
    }

    pub(in crate::node) fn process_file_events_reception(
        &self,
        conv: ConversationId,
        context: &'static str,
    ) {
        let result = (|| -> Result<(), NodeError> {
            let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
            let all_events: Vec<Event> = {
                let log = self.log.lock().expect("log mutex not poisoned");
                let events = log.events(&conv)?;
                if !events.iter().any(|event| {
                    event.kind == EventKind::FileManifest && event.author != self_author
                }) {
                    drop(log);
                    let file = self
                        .files
                        .lock()
                        .expect("files lock not poisoned")
                        .manifest(&conv)
                        .is_some();
                    if file {
                        self.persist_media_if_complete(conv);
                        self.note_file_change();
                    }
                    return Ok(());
                }
                events
            };
            // Is this a channel conversation? If so, open with its group key.
            let is_channel = {
                let book = self.channels.lock().expect("channels mutex not poisoned");
                book.state(&conv).is_some()
            };
            if is_channel
                && !self
                    .pending_channel_plaintext
                    .lock()
                    .expect("pending plaintext lock not poisoned")
                    .messages
                    .is_empty()
            {
                return Ok(());
            }
            let content_gate = self.channel_content_gate(conv)?;
            let mut surfaced: Vec<ReceivedFile> = Vec::new();
            for event in all_events.into_iter().filter(|event| {
                event.kind == EventKind::FileManifest && event.author != self_author
            }) {
                if self
                    .account_suppression
                    .lock()
                    .expect("account suppression lock not poisoned")
                    .contains(conv, event.id)
                {
                    continue;
                }
                if content_gate
                    .as_ref()
                    .is_some_and(|ids| !ids.contains(&event.id))
                {
                    continue;
                }
                if self
                    .files
                    .lock()
                    .expect("files mutex not poisoned")
                    .is_emitted(&event.id)
                {
                    continue;
                }
                if !is_channel
                    && self
                        .historical_author(event.author.ed25519_pub())
                        .is_some_and(|proof| self.certified_sibling(&proof.public()))
                {
                    // Sibling file metadata and bytes use the category-negotiated account path.
                    continue;
                }
                // NB: we mark emitted only AFTER a successful open+decode (below), so a
                // manifest we can't open yet — e.g. a DM whose sender isn't in the roster
                // until discovery catches up — is retried on a later sync, not lost.
                let plaintext = if is_channel {
                    let mut book = self.channels.lock().expect("channels mutex not poisoned");
                    match book.state_mut(&conv).and_then(|s| {
                        if s.is_accepted_member(&event.author.user_id()) {
                            s.open_sender_message(&event.author.user_id(), &event.ciphertext)
                        } else {
                            None
                        }
                    }) {
                        Some(p) => p,
                        None => continue,
                    }
                } else {
                    let sender_x25519 = {
                        match self.historical_author(event.author.ed25519_pub()) {
                            Some(p)
                                if conv
                                    == super::conversation::dm_conversation_id(
                                        &self.identity.public(),
                                        &p.public(),
                                    ) =>
                            {
                                p.x25519_pub
                            }
                            _ => continue, // unknown author or incorrect device pair
                        }
                    };
                    match crate::dm::open(&self.identity, &sender_x25519, &event.ciphertext) {
                        Ok(p) => p,
                        Err(_) => continue,
                    }
                };
                let own_device = self
                    .linked_devices()
                    .iter()
                    .any(|device| device.public.user_id() == event.author.user_id());
                if self.recipient_expired(&plaintext, event.id, event.wall_clock) {
                    self.files
                        .lock()
                        .expect("files mutex not poisoned")
                        .mark_emitted(event.id);
                    continue;
                }
                let Some(manifest) = super::files::validated_manifest(&plaintext) else {
                    continue;
                };
                if !self
                    .files
                    .lock()
                    .expect("files lock not poisoned")
                    .accepts(&manifest)
                {
                    continue;
                }
                let received = ReceivedFile {
                    protection: manifest.protection(),
                    expires_at_ms: crate::expiry::deadline(&plaintext),
                    conv,
                    is_channel,
                    from: event.author.user_id(),
                    name: if manifest.protection().is_some() {
                        "Protected image".into()
                    } else {
                        manifest.name().to_string()
                    },
                    size: manifest.size(),
                    mime: manifest.mime().to_string(),
                    file_conv: manifest.file_conv(),
                    media: crate::node::media_store::manifest_is_media(&manifest),
                    event_id: event.id,
                    authored_at: event.wall_clock,
                    received_at: super::node::now_millis(),
                    arrival_kind: context,
                    own_device,
                    batch_id: None,
                    batch_complete: true,
                    batch_size: 1,
                };
                // The conversation to FILE this manifest under for history. A channel manifest
                // stays in the channel conv (which is the UI's conversation id). A DM manifest
                // is account-addressed in this app: map it to the ACCOUNT conversation with the
                // author's certified account (mirroring `emit_new_messages`), so it lines up
                // with the account-keyed conversation view on both sides. A DM author with no
                // known account (or a self-synced copy from our own device) falls back to the
                // raw conv — it still shows in the tray, just not folded into account history.
                let host_conv = if is_channel {
                    conv
                } else {
                    let peer_account = self
                        .historical_author(event.author.ed25519_pub())
                        .and_then(|p| p.account_id());
                    let my_account = self.account.account_id();
                    match peer_account {
                        Some(acct) if acct != my_account => {
                            account_conversation_id(&my_account, &acct)
                        }
                        _ => conv,
                    }
                };
                if self
                    .delivery
                    .lock()
                    .expect("delivery lock not poisoned")
                    .manifest_event_erased(event.id)
                {
                    continue;
                }
                // Persist the surfaced manifest durably so the file book's emitted set + this
                // manifest survive a restart — WITHOUT the old bug of marking never-opened
                // manifests emitted (which lost the file). Best-effort: a failure here at worst
                // re-surfaces the file after restart, never loses it.
                // Key by the HOST (DM/channel/account) conversation, not the per-file conv, so
                // `conversation_files` can list a conversation's files in time order for
                // history. Startup FileBook seeding iterates all entries regardless of key.
                let entry = super::received_log::ReceivedEntry {
                    event_id: event.id,
                    conversation: host_conv,
                    from: event.author.user_id(),
                    wall_clock: event.wall_clock,
                    plaintext,
                };
                let installed = if is_channel {
                    self.received_files
                        .lock()
                        .expect("files lock not poisoned")
                        .record_durable(&entry)
                } else {
                    match self.historical_author(event.author.ed25519_pub()) {
                        Some(proof) => self.accept_manifest_event(&event, &proof, entry),
                        None => continue,
                    }
                };
                if installed.is_err() {
                    continue;
                }
                if !is_channel {
                    if let Err(error) = self.complete_sticker_aliases(manifest.file_conv()) {
                        log::warn!("sticker completion deferred: {error}");
                    }

                    continue;
                } // live journal recovery owns DM file publication
                if !own_device {
                    self.note_account_local_change();
                }
                self.remember_manifest_scope(manifest.file_conv(), conv, event.author, event.id);
                {
                    let mut files = self.files.lock().expect("files mutex not poisoned");
                    files.mark_emitted(event.id);
                    files.record_event(event.id, event.author, manifest);
                }
                surfaced.push(received);
            }
            let total = surfaced.len();
            let kind = if context == "live" && total > 1 {
                "catch_up"
            } else {
                context
            };
            let batch = (kind == "catch_up")
                .then(|| {
                    surfaced
                        .last()
                        .map(|file| format!("file:{}", file.event_id.to_hex()))
                })
                .flatten();
            for (index, mut rf) in surfaced.into_iter().enumerate() {
                rf.arrival_kind = kind;
                rf.batch_id = batch.clone();
                rf.batch_complete = index + 1 == total;
                rf.batch_size = total;
                // We have the manifest; if its chunks haven't all arrived, queue a direct pull
                // from peers (the sender's one-shot push may have raced our discovery, and on a
                // PO-less LAN nothing else fetches them). Cleared once the file completes.
                let fc = rf.file_conv;
                if match self.attachment_consent_status(fc) {
                    Ok(status) => status,
                    Err(error) => {
                        log::warn!("attachment consent lookup deferred: {error}");
                        continue;
                    }
                } == Some(super::attachment_consent::ReceiveDecision::Accepted)
                    && matches!(self.file_progress(fc)?, Some(p) if p.done < p.total)
                {
                    self.pending_files
                        .lock()
                        .expect("pending_files mutex not poisoned")
                        .insert(fc);
                    self.file_pull_notify.notify_one();
                } else {
                    // All chunks already arrived with the manifest (the push landed): if this is
                    // a media file, persist it into the durable store now (and prune its chunks).
                    self.persist_media_if_complete(fc);
                }
                let _ = self.file_incoming.send(rf);
            }

            Ok(())
        })();
        if let Err(error) = result {
            log::warn!("process_file_events_reception deferred after storage failure: {error}");
        }
    }

    /// Fetch chunks for any file we've surfaced a manifest for but don't fully hold yet,
    /// pulling DIRECTLY from connected peers. The post-office drain covers file convs only
    /// when a post office exists; on a plain peer-to-peer LAN the sender's one-shot push is
    /// otherwise the only delivery, so a push that missed (discovery race) would leave the
    /// recipient stuck at 0 chunks. Best-effort + idempotent: a complete file is dropped
    /// from the pending set; an unreachable peer is just skipped and retried next tick.
    /// Fetch only from the signed sender or accepted sharing members. Work is
    /// bounded across files and sources; an unresponsive holder cannot pin the queue.
    pub async fn pull_pending_files(&self) {
        let result: Result<(), NodeError> = async {
            use futures_util::stream::{self, StreamExt};
            let Ok(_pull) = self.file_pull_gate.try_lock() else {
                return Ok(());
            };
            let mut pending: Vec<_> = self
                .pending_files
                .lock()
                .expect("pending files mutex not poisoned")
                .iter()
                .copied()
                .collect();
            pending.sort_unstable_by_key(|id| *id.as_bytes());
            if pending.is_empty() {
                return Ok(());
            }
            let cursor = self
                .file_pull_cursor
                .fetch_add(8, std::sync::atomic::Ordering::Relaxed);
            let offset = cursor % pending.len();
            pending.rotate_left(offset);
            pending.truncate(8);
            stream::iter(pending)
                .for_each_concurrent(2, |fc| async move {
                    let result: Result<(), NodeError> = async {
                        if match self.attachment_consent_status(fc) {
                            Ok(status) => status,
                            Err(error) => {
                                log::warn!("attachment consent lookup deferred: {error}");
                                return Ok(());
                            }
                        } != Some(super::attachment_consent::ReceiveDecision::Accepted)
                        {
                            self.pending_files
                                .lock()
                                .expect("pending files mutex not poisoned")
                                .remove(&fc);
                            return Ok(());
                        }
                        if self.media.contains(fc) && self.has_verified_file_completion(fc) {
                            let progressed = match self.complete_sticker_aliases(fc) {
                                Ok(progressed) => progressed,
                                Err(error) => {
                                    log::warn!("sticker completion deferred: {error}");
                                    return Ok(());
                                }
                            };
                            let pending = self.pending_sticker_aliases(fc);
                            if !pending {
                                self.pending_files
                                    .lock()
                                    .expect("pending files mutex not poisoned")
                                    .remove(&fc);
                            }
                            if progressed || !pending {
                                return Ok(());
                            }
                        }
                        if self.file_progress(fc)?.is_none_or(|p| p.done < p.total) {
                            let candidates = match self.file_recovery_sources(fc, cursor / 8) {
                                Ok(candidates) => candidates,
                                Err(error) => {
                                    log::warn!("file recovery deferred: {error}");
                                    return Ok(());
                                }
                            };
                            let mut pulls = stream::iter(candidates)
                                .map(|peer| async move {
                                    let _ = tokio::time::timeout(
                                        super::delivery_runtime::FILE_EXCHANGE_TIMEOUT,
                                        async {
                                            let Ok(mut channel) = self
                                                .privacy_sync_dial(peer.addr, &peer.public)
                                                .await
                                            else {
                                                return;
                                            };
                                            if match self.attachment_consent_status(fc) {
                                        Ok(status) => status,
                                        Err(error) => {
                                            log::warn!(
                                                "attachment consent lookup deferred: {error}"
                                            );
                                            return;
                                        }
                                    } != Some(
                                        super::attachment_consent::ReceiveDecision::Accepted,
                                    ) {
                                        return;
                                    }
                                    let store = self.sync_store(channel.peer_identity());
                                    let manifests = self.files.lock().expect("files lock").manifest_events(fc);
                                    let mut offered_manifest = None;
                                    for manifest in manifests {
                                        match self.incoming_offer(fc, Some(manifest), Some(&peer.public)) {
                                            Ok(Some(_)) => { offered_manifest = Some(manifest); break; },
                                            Ok(None) => {},
                                            Err(error) => { log::warn!("file offer lookup deferred: {error}"); return; }
                                        }
                                    }
                                    if let Some(manifest) = offered_manifest
                                        && self.exchange_file_message_identities(&mut channel, vec![(fc, manifest)], vec![]).await.is_err() {
                                        drop(channel);
                                        let Ok(reconnected) = self.privacy_sync_dial(peer.addr, &peer.public).await else { return; };
                                        channel = reconnected;
                                    }
                                    let sticker_offer = match self.sticker_recovery_offer(fc, &peer.public) {
                                        Ok(offer) => offer,
                                        Err(error) => { log::warn!("sticker recovery deferred: {error}"); return; }
                                    };
                                    if let Some(manifest) = sticker_offer {
                                        match super::session::request_sticker_file_proof(&mut channel, fc, manifest).await {
                                            Ok(Some(headers)) => {
                                                use crate::eventlog::sync::SyncStore;
                                                if let Err(error) = store.lock().expect("sync store lock").sticker_file_consent(&fc, &manifest, headers) {
                                                    log::warn!("sticker consent deferred: {error}"); return;
                                                }
                                                if self.media.contains(fc) && self.has_verified_file_completion(fc) && !self.pending_sticker_aliases(fc) {
                                                    self.recycle_sync_channel(channel);
                                                    return;
                                                }
                                            }
                                            Ok(None) => {}
                                            Err(_) => {
                                                drop(channel);
                                                let Ok(reconnected) = self.privacy_sync_dial(peer.addr, &peer.public).await else { return; };
                                                channel = reconnected;
                                                if !matches!(self.attachment_consent_status(fc), Ok(Some(super::attachment_consent::ReceiveDecision::Accepted))) {
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                            if super::session::request_file_round_paged(
                                                &mut channel,
                                                &store,
                                                fc,
                                            )
                                            .await
                                            .is_ok()
                                            {
                                                self.recycle_sync_channel(channel);
                                            }
                                        },
                                    )
                                    .await;
                                })
                                .buffer_unordered(2);
                            while pulls.next().await.is_some() {
                                self.note_file_change();
                                if self.file_progress(fc)?.is_some_and(|p| p.done >= p.total) {
                                    break;
                                }
                            }
                        }
                        if self.file_progress(fc)?.is_some_and(|p| p.done >= p.total) {
                            self.persist_media_if_complete(fc);
                            let managed = self
                                .files
                                .lock()
                                .expect("files lock not poisoned")
                                .manifest(&fc)
                                .is_some_and(super::media_store::manifest_is_media);
                            if !managed
                                || (self.media.contains(fc)
                                    && self.has_verified_file_completion(fc))
                            {
                                self.pending_files
                                    .lock()
                                    .expect("pending files mutex not poisoned")
                                    .remove(&fc);
                            } else {
                                // The byte store may succeed before completion metadata.
                                // Its helper clears the transient queue; restore recovery
                                // ownership until both durable writes have succeeded.
                                self.pending_files
                                    .lock()
                                    .expect("pending files mutex not poisoned")
                                    .insert(fc);
                            }
                            self.note_file_change();
                        }
                        Ok(())
                    }
                    .await;
                    if let Err(error) = result {
                        log::warn!("file receive deferred after storage failure: {error}");
                    }
                })
                .await;

            Ok(())
        }
        .await;
        if let Err(error) = result {
            log::warn!("pull_pending_files deferred after storage failure: {error}");
        }
    }

    fn file_recovery_sources(
        &self,
        file: ConversationId,
        cursor: usize,
    ) -> Result<Vec<PeerRecord>, NodeError> {
        let (authors, manifests) = {
            let files = self.files.lock().expect("files lock not poisoned");
            (files.authors(file), files.manifest_events(file))
        };
        let parents: Vec<_> = {
            let log = self.log.lock().expect("log lock not poisoned");
            let mut parents = Vec::new();
            for id in &manifests {
                if let Some(event) = log.get(id)? {
                    parents.push(event.conversation_id);
                }
            }
            parents
        };
        let mut identities = authors
            .into_iter()
            .filter_map(|author| {
                self.historical_author(author.ed25519_pub())
                    .map(|proof| proof.public())
            })
            .collect::<Vec<_>>();
        {
            let book = self.channels.lock().expect("channels lock not poisoned");
            let mut members = Vec::new();
            let mut channel_bound = false;
            for parent in parents {
                if let Some(state) = book.state(&parent) {
                    channel_bound = true;
                    members.extend(
                        state
                            .members()
                            .iter()
                            .filter(|member| {
                                state.is_accepted_member(&member.user_id())
                                    && book.replica_enabled(parent, &member.user_id())
                            })
                            .cloned(),
                    );
                }
            }
            if channel_bound {
                identities = members;
            }
        }
        let mut peers = identities
            .into_iter()
            .filter(|identity| *identity != self.identity.public())
            .filter_map(|identity| {
                self.routing_peer(&identity.user_id())
                    .filter(|peer| !peer.post_office && peer.public == identity)
            })
            .collect::<Vec<_>>();
        peers.sort_unstable_by_key(|peer| peer.public.user_id());
        peers.dedup_by_key(|peer| peer.public.user_id());
        if !peers.is_empty() {
            let offset = cursor % peers.len();
            peers.rotate_left(offset);
        }
        peers.truncate(3);
        Ok(peers)
    }

    /// Run the channel book over `conv`'s events and stream any newly-decryptable
    /// channel messages. A no-op for DM conversations (no channel state).
    pub(in crate::node) fn process_channel(&self, conv: ConversationId) {
        self.process_channel_reception(conv, "live");
    }

    /// Recovery provenance is supplied by the operation, never inferred from a
    /// sender's wall clock. A multi-message decryption pass is a catch-up batch.
    pub(in crate::node) fn process_channel_reception(
        &self,
        conv: ConversationId,
        context: &'static str,
    ) {
        let result = (|| -> Result<(), NodeError> {
            let mut pending = self
                .pending_channel_plaintext
                .lock()
                .expect("pending plaintext lock not poisoned");
            if pending
                .retry_at
                .is_some_and(|deadline| std::time::Instant::now() < deadline)
                || pending
                    .messages
                    .first()
                    .is_some_and(|message| message.channel_id != conv)
            {
                return Ok(());
            }
            let mut saved: std::collections::HashSet<_> = self
                .received
                .lock()
                .expect("received mutex not poisoned")
                .entries_ref(&conv)
                .iter()
                .map(|entry| entry.event_id)
                .collect();
            let events: Vec<Event> = self
                .log
                .lock()
                .expect("log mutex not poisoned")
                .events(&conv)?
                .into_iter()
                .collect();
            let refs: Vec<_> = events.iter().collect();
            let mut replay = super::channel::ChannelReplay::new(&refs);
            let mut context = context;
            loop {
                let retrying = !pending.messages.is_empty();
                let (mut messages, limited) = if retrying {
                    context = "catch_up";
                    (std::mem::take(&mut pending.messages), false)
                } else {
                    let mut book = self.channels.lock().expect("channels mutex not poisoned");
                    let before = book.info_projection(conv);
                    let result = book.process_replay(
                        &self.identity,
                        conv,
                        &mut replay,
                        CHANNEL_PLAINTEXT_BATCH,
                        super::channel::CHANNEL_PENDING_BYTES.saturating_sub(
                            pending
                                .terminal
                                .as_ref()
                                .map_or(0, super::channel::ReceivedChannelMessage::retained_bytes),
                        ),
                    );
                    if before != book.info_projection(conv) {
                        let _ = self.channel_changes.send(conv);
                    }
                    result
                };
                let own_devices: std::collections::HashSet<_> = self
                    .linked_devices()
                    .iter()
                    .map(|device| device.public.user_id())
                    .collect();
                let observed_at = super::node::now_millis();
                for msg in &mut messages {
                    if msg.received_at == 0 {
                        msg.received_at = observed_at;
                    }
                    if !own_devices.contains(&msg.from) {
                        let _ = self.recipient_expired(&msg.wrapped, msg.event_id, msg.wall_clock);
                    }
                }
                let opened = messages.len();
                let mut received = Vec::new();
                let mut messages = messages.into_iter();
                while let Some(mut msg) = messages.next() {
                    if saved.contains(&msg.event_id)
                        || self
                            .account_suppression
                            .lock()
                            .expect("account suppression lock not poisoned")
                            .contains(conv, msg.event_id)
                    {
                        continue;
                    }
                    msg.own_device = own_devices.contains(&msg.from);
                    if self.recipient_expired(&msg.wrapped, msg.event_id, msg.wall_clock) {
                        continue;
                    }
                    if msg.received_at == 0 {
                        msg.received_at = super::node::now_millis();
                    }
                    if let Err(error) = self
                        .received
                        .lock()
                        .expect("received mutex not poisoned")
                        .record(
                            conv,
                            msg.from.clone(),
                            msg.wall_clock,
                            &msg.wrapped,
                            msg.event_id,
                        )
                    {
                        log::warn!(
                            "channel plaintext persistence failed; retaining bounded retry: {error}"
                        );
                        pending.messages.push(msg);
                        pending.messages.extend(messages);
                        break;
                    }
                    saved.insert(msg.event_id);
                    if !msg.own_device && msg.from != self.user_id() {
                        self.note_account_local_change();
                    }
                    received.push(msg);
                }
                let (history, received): (Vec<_>, Vec<_>) = received
                    .into_iter()
                    .partition(|message| message.arrival_kind == "history_sync");
                for message in history {
                    let _ = self.channel_incoming.send(message);
                }
                let total = received.len();
                let kind = if context == "live" && (opened > 1 || retrying) {
                    "catch_up"
                } else {
                    context
                };
                if kind == "catch_up" {
                    if pending.batch_id.is_none() {
                        pending.batch_id = received
                            .first()
                            .or_else(|| pending.messages.first())
                            .map(|msg| format!("message:{}", msg.event_id.to_hex()));
                    }
                    pending.batch_size += total;
                    if pending.terminal.is_none() {
                        pending.terminal = received
                            .first()
                            .or_else(|| pending.messages.first())
                            .map(|msg| msg.terminal(pending.batch_id.clone(), pending.batch_size));
                    }
                    for mut msg in received {
                        msg.arrival_kind = kind;
                        msg.batch_id = pending.batch_id.clone();
                        msg.batch_complete = false;
                        msg.batch_size = pending.batch_size;
                        pending.terminal =
                            Some(msg.terminal(pending.batch_id.clone(), pending.batch_size));
                        let _ = self.channel_incoming.send(msg);
                    }
                } else {
                    for mut msg in received {
                        msg.arrival_kind = kind;
                        msg.batch_complete = true;
                        msg.batch_size = total;
                        let _ = self.channel_incoming.send(msg);
                    }
                }
                if total > 0 {
                    self.schedule_channel_recovery(conv);
                }
                if !pending.messages.is_empty() {
                    let delay = pending.delay_ms.max(1000);
                    pending.retry_at =
                        Some(std::time::Instant::now() + Duration::from_millis(delay));
                    pending.delay_ms = (delay * 2).min(30_000);
                    self.channel_recovery_notify.notify_one();
                    return Ok(());
                }
                pending.retry_at = None;
                pending.delay_ms = 0;
                if !limited && !retrying {
                    break;
                }
                context = "catch_up";
            }
            if let Some(mut terminal) = pending.terminal.take() {
                terminal.batch_size = pending.batch_size;
                let _ = self.channel_incoming.send(terminal);
            }
            pending.batch_id = None;
            pending.batch_size = 0;
            // ChannelBook returns only newly validated, decryptable messages. A
            // coalesced domain-specific wake advances bounded background gossip;
            // repeated processing and empty sync rounds do not create feedback.
            let has_deletes = self
                .log
                .lock()
                .expect("log mutex not poisoned")
                .events(&conv)?
                .iter()
                .any(|event| event.kind == EventKind::Delete);
            if has_deletes && let Err(error) = self.recalled_targets_channel(conv) {
                log::warn!("channel recall projection deferred: {error}");
            }

            Ok(())
        })();
        if let Err(error) = result {
            log::warn!("process_channel_reception deferred after storage failure: {error}");
        }
    }

    /// Retry exact failed plaintext even with no reachable member or new event.
    pub(in crate::node) fn retry_channel_plaintext(&self) {
        let channel = {
            let pending = self
                .pending_channel_plaintext
                .lock()
                .expect("pending plaintext lock not poisoned");
            if pending
                .retry_at
                .is_some_and(|deadline| std::time::Instant::now() < deadline)
            {
                return;
            }
            pending.messages.first().map(|message| message.channel_id)
        };
        if let Some(channel) = channel {
            self.process_channel_reception(channel, "catch_up");
            self.process_file_events_reception(channel, "catch_up");
        }
    }

    /// Receive producer owned by the participant runtime task set. A healthy
    /// relay keeps its own cadence while one alternative makes bounded progress.
    pub(in crate::node) async fn run_post_office_drains(self: Arc<Self>) {
        use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};
        let mut network = self.network_changes.subscribe();
        let mut roster = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .subscribe_changes();
        loop {
            let mut candidates = self.post_office_drain_candidates();
            let generation = self
                .privacy
                .generation
                .load(std::sync::atomic::Ordering::SeqCst);
            let mut schedule = PostOfficeDrainSchedule::default();
            let mut active = Vec::new();
            let mut cancellation = Vec::new();
            let mut attempts = FuturesUnordered::new();
            loop {
                let eligible = candidates
                    .iter()
                    .filter(|po| {
                        self.relay_receive_allowed(&po.public) && self.relay_retry_due(&po.public)
                    })
                    .map(|po| (self.peer_endpoint_key(po.addr, &po.public), po.clone()))
                    .collect::<Vec<_>>();
                while let Some(index) =
                    schedule.select(&eligible, &active, tokio::time::Instant::now())
                {
                    let (key, po) = eligible[index].clone();
                    active.push(key.clone());
                    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
                    cancellation.push((key.clone(), abort));
                    let node = &self;
                    attempts.push(
                        async move {
                            let result = futures_util::future::Abortable::new(
                                async {
                                    if node.cached_peer_snapshot_async().await.is_err() {
                                        log::warn!("verified discovery cache update failed");
                                    }
                                    let peers = node.post_office_drain_peers();
                                    if peers.is_empty() {
                                        None
                                    } else {
                                        Some(node.drain_one_post_office(&po, &peers).await)
                                    }
                                },
                                registration,
                            )
                            .await;
                            (key, result)
                        }
                        .boxed(),
                    );
                }
                let wake = schedule.next_wake(&eligible, &active, tokio::time::Instant::now());
                tokio::select! {
                    result = attempts.next(), if !attempts.is_empty() => {
                        if let Some((key, result)) = result {
                            active.retain(|candidate| candidate != &key);
                            cancellation.retain(|(candidate, _)| candidate != &key);
                            if let Ok(result) = result {
                                schedule.completed(key, result, tokio::time::Instant::now());
                            }
                        }
                    }
                    _ = tokio::time::sleep_until(wake) => {},
                    // Mailbox switches own a retained wake rather than competing
                    // with delivery's Notify or waiting for the idle cadence.
                    _ = self.post_office_receive_notify.notified() => break,
                    _ = network.changed() => break,
                    _ = roster.changed() => {},
                }
                // Roster churn must not repeatedly cancel a valid slow relay.
                // Retire only the exact identities/endpoints that lost authority;
                // network and policy generations retire the whole owned work set.
                if generation
                    != self
                        .privacy
                        .generation
                        .load(std::sync::atomic::Ordering::SeqCst)
                {
                    break;
                }
                candidates = self.post_office_drain_candidates();
                let current = candidates
                    .iter()
                    .map(|po| self.peer_endpoint_key(po.addr, &po.public))
                    .collect::<Vec<_>>();
                for (key, abort) in &cancellation {
                    if !current.contains(key) {
                        abort.abort();
                    }
                }
                schedule.due.retain(|(key, _)| current.contains(key));
                if schedule
                    .primary
                    .as_ref()
                    .is_some_and(|key| !current.contains(key))
                {
                    schedule.primary = None;
                }
            }
            // Futures are owned here: cancellation/reset drops every socket and
            // in-flight exchange before the next generation starts.
        }
    }

    fn post_office_drain_candidates(&self) -> Vec<PeerRecord> {
        let candidates = {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            self.post_office_candidates(&roster)
        };
        candidates
            .into_iter()
            .filter(|po| self.relay_receive_allowed(&po.public))
            .collect()
    }

    /// Pull from every known post office. Mailboxes are relay-specific, so a
    /// message accepted during failover may remain on the fallback relay after
    /// the preferred one returns.
    pub async fn drain_from_post_office(&self) {
        if self.cached_peer_snapshot_async().await.is_err() {
            log::warn!("verified discovery cache update failed");
        }
        let post_offices = {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            self.post_office_candidates(&roster)
        };
        use futures_util::stream::{self, StreamExt};
        let peers = self.post_office_drain_peers();
        if peers.is_empty() {
            return;
        }
        let post_offices = self.relay_candidate_batch("drain:relays", post_offices);
        stream::iter(post_offices)
            .for_each_concurrent(2, |po| {
                let peers = &peers;
                async move {
                    self.drain_one_post_office(&po, peers).await;
                }
            })
            .await;
    }

    fn post_office_drain_peers(&self) -> Vec<crate::discovery::Announce> {
        let mut peers = self
            .historical_dm_peers()
            .into_iter()
            .filter(|peer| self.verified_dm_pull_peer(&peer.public().user_id()))
            .collect::<Vec<_>>();
        peers.sort_unstable_by_key(|peer| peer.public().user_id());
        peers
    }

    async fn drain_one_post_office(
        &self,
        po: &PeerRecord,
        peers: &[crate::discovery::Announce],
    ) -> bool {
        if !self.relay_receive_allowed(&po.public) || !self.relay_retry_due(&po.public) {
            return false;
        }
        let relay_id = po.public.user_id();
        let start = self
            .relay_drain_cursor
            .lock()
            .expect("relay cursor lock not poisoned")
            .get(&relay_id)
            .copied()
            .unwrap_or(0);
        let mut channel = None;
        let mut success = true;
        for offset in 0..peers.len().min(8) {
            let index = start.wrapping_add(offset) % peers.len();
            // Record the turn before awaiting. Neither a huge mailbox
            // nor a silent relay can pin all later mailbox scopes.
            {
                let mut cursors = self
                    .relay_drain_cursor
                    .lock()
                    .expect("relay cursor lock not poisoned");
                if cursors.len() >= 256
                    && !cursors.contains_key(&relay_id)
                    && let Some(key) = cursors.keys().next().cloned()
                {
                    cursors.remove(&key);
                }
                cursors.insert(relay_id.clone(), index.wrapping_add(1));
            }
            let peer = &peers[index];
            if !self.verified_dm_pull_peer(&peer.public().user_id()) {
                continue;
            }
            let attempt = async {
                if channel.is_none() {
                    channel = Some(
                        self.privacy_sync_dial(po.addr, &po.public)
                            .await
                            .map_err(super::session::SessionError::Transport)?,
                    );
                }
                if !self.relay_receive_allowed(&po.public) {
                    return Err(super::session::SessionError::UnexpectedMessage);
                }
                self.opaque_relay_round(channel.as_mut().unwrap(), &peer.public(), None)
                    .await
                    .map(|_| ())
            };
            if !matches!(
                tokio::time::timeout(super::delivery_runtime::RELAY_EXCHANGE_TIMEOUT, attempt)
                    .await,
                Ok(Ok(()))
            ) {
                success = false;
                channel = None;
                break;
            }
        }
        if let Some(channel) = channel {
            self.recycle_sync_channel(channel);
        }
        self.record_relay_attempt(&po.public, success);
        success
    }

    /// Decrypt (via the Double Ratchet) and emit any not-yet-emitted, non-self
    /// `Message` events in `conv`. A message is marked emitted ONLY after it
    /// successfully decrypts and is recorded — so a transiently-undecryptable one
    /// (author not yet in the roster, or its ratchet key not yet derivable) is
    /// retried on a later sync rather than lost.
    pub(in crate::node) fn emit_new_messages(&self, conv: ConversationId) {
        let result = (|| -> Result<(), NodeError> {
            let mut delivery = self.delivery.lock().expect("delivery lock not poisoned");
            if let Err(error) = self.recover_delivery(&mut delivery) {
                log::warn!("DM processing awaits local delivery recovery: {error}");
                return Ok(());
            }
            let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
            let candidates: Vec<Event> = {
                let log = self.log.lock().expect("log mutex not poisoned");
                let emitted = self.emitted.lock().expect("emitted mutex not poisoned");
                log.events(&conv)?
                    .into_iter()
                    .filter(|e| {
                        e.kind == EventKind::Message
                            && e.author != self_author
                            && !emitted.contains(&e.id)
                    })
                    .collect()
            };
            for event in candidates {
                if let Some(proof) = self.historical_author(event.author.ed25519_pub()) {
                    let _ = self.accept_dm_event(&event, &proof, &mut delivery)?;
                } else if let Some(device) = self
                    .linked_devices()
                    .into_iter()
                    .find(|device| device.public.ed25519_pub == *event.author.ed25519_pub())
                {
                    let _ = self.accept_dm_event_with_author(
                        &event,
                        &device.public,
                        None,
                        &mut delivery,
                    )?;
                }
            }
            drop(delivery);
            if let Err(error) = self.project_private_dm_controls(conv) {
                log::warn!("private DM controls await durable projection: {error}");
            }
            // A recall may arrive before its target is viewed. Persist verified
            // tombstones while the old device and peer proofs are still available,
            // so an offline backup cannot resurrect the recalled plaintext.
            if self
                .log
                .lock()
                .expect("log mutex not poisoned")
                .events(&conv)?
                .iter()
                .any(|event| event.kind == EventKind::Delete)
            {
                let my_account = self.account_id();
                let peers = self.historical_dm_peers();
                let matched: Vec<_> = peers
                    .iter()
                    .filter(|peer| {
                        super::conversation::dm_conversation_id(
                            &self.identity.public(),
                            &peer.public(),
                        ) == conv
                    })
                    .filter_map(|peer| peer.account_id())
                    .collect();
                for account in matched {
                    if account == my_account {
                        for other in self.known_account_ids() {
                            self.recalled_targets_account(&other)?;
                        }
                    } else {
                        self.recalled_targets_account(&account)?;
                    }
                }
            }

            Ok(())
        })();
        if let Err(error) = result {
            log::warn!("emit_new_messages deferred after storage failure: {error}");
        }
    }
}

#[cfg(test)]
mod post_office_drain_schedule_tests {
    use super::*;
    use crate::identity::{account::Account, device::DeviceIdentity};

    fn candidates(node: &Node, count: u16) -> Vec<(DrainKey, PeerRecord)> {
        (1..=count)
            .map(|port| {
                let identity = DeviceIdentity::generate();
                let proof = crate::discovery::Announce::new_post_office(&identity, "Relay", port);
                let mut roster = crate::discovery::Roster::default();
                roster.update(&proof, "127.0.0.1".parse().unwrap(), &node.user_id());
                let peer = roster.peers().into_iter().next().unwrap();
                (node.peer_endpoint_key(peer.addr, &peer.public), peer)
            })
            .collect()
    }

    fn node(dir: &std::path::Path) -> Arc<Node> {
        let peer = crate::discovery::Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "Peer",
            9,
        );
        super::super::test_support::node_with_files(
            dir,
            DeviceIdentity::generate(),
            Account::generate(),
            &peer,
        )
        .0
    }

    #[test]
    fn primary_reserves_capacity_while_alternatives_rotate_without_duplicate_exchanges() {
        let dir = tempfile::tempdir().unwrap();
        let node = node(dir.path());
        let candidates = candidates(&node, 4);
        let now = tokio::time::Instant::now();
        let mut schedule = PostOfficeDrainSchedule::default();
        assert_eq!(schedule.select(&candidates, &[], now), Some(0));
        let mut active = vec![candidates[0].0.clone()];
        assert_eq!(schedule.select(&candidates, &active, now), Some(1));
        active.push(candidates[1].0.clone());
        assert_eq!(schedule.select(&candidates, &active, now), None);
        active.pop();
        schedule.completed(candidates[1].0.clone(), Some(true), now);
        assert_eq!(
            schedule.select(&candidates, &active, now),
            None,
            "a third office cannot take the idle healthy office's slot"
        );
        assert_eq!(
            schedule.next_wake(&candidates, &active, now),
            now + POST_OFFICE_DRAIN_INTERVAL
        );
        let next = now + POST_OFFICE_DRAIN_INTERVAL;
        assert_eq!(schedule.select(&candidates, &active, next), Some(1));
        active.clear();
        schedule.completed(candidates[0].0.clone(), Some(true), next);
        schedule.completed(candidates[1].0.clone(), Some(true), next);
        assert_eq!(schedule.select(&candidates, &active, next), Some(2));
        schedule.completed(candidates[2].0.clone(), Some(true), next);
        assert_eq!(schedule.select(&candidates, &active, next), Some(3));
        schedule.completed(candidates[1].0.clone(), Some(false), next);
        assert!(
            schedule.primary.is_none(),
            "an actual failure yields the primary role"
        );
    }

    #[test]
    fn bounded_due_metadata_never_discards_a_live_cadence_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let node = node(dir.path());
        let candidates = candidates(&node, MAX_DRAIN_DUE_ENTRIES as u16 + 1);
        let now = tokio::time::Instant::now();
        let mut schedule = PostOfficeDrainSchedule::default();
        for (key, _) in &candidates[..MAX_DRAIN_DUE_ENTRIES] {
            schedule.completed(key.clone(), None, now);
        }
        assert_eq!(schedule.due.len(), MAX_DRAIN_DUE_ENTRIES);
        assert_eq!(schedule.select(&candidates, &[], now), None);
        assert_eq!(
            schedule.next_wake(&candidates, &[], now),
            now + POST_OFFICE_DRAIN_INTERVAL
        );
        assert!(
            schedule
                .select(&candidates, &[], now + POST_OFFICE_DRAIN_INTERVAL)
                .is_some()
        );
        assert!(
            schedule.due.is_empty(),
            "expired cadence hints require no background cleanup"
        );
    }
}

#[cfg(test)]
mod file_recovery_tests {
    use crate::discovery::Announce;
    use crate::identity::{account::Account, device::DeviceIdentity};

    #[tokio::test]
    async fn attachment_recovery_excludes_unrelated_roster_peers() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let aa = Account::generate();
        let bob_account = Account::generate();
        let ap = Announce::new_with_account(&alice, &aa, "Alice", 9);
        let bp = Announce::new_with_account(&bob, &bob_account, "Bob", 9);
        let (a, _, _) = super::super::test_support::node_with_files(source.path(), alice, aa, &bp);
        let (b, _, _) =
            super::super::test_support::node_with_files(target.path(), bob, bob_account, &ap);
        let unrelated = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "Unrelated",
            9,
        );
        b.roster
            .lock()
            .unwrap()
            .update(&unrelated, "127.0.0.1".parse().unwrap(), &b.user_id());
        let path = source.path().join("source.bin");
        std::fs::write(&path, b"source aware recovery").unwrap();
        let (id, file) = a
            .enqueue_file_to_account(&b.account_id(), &path, crate::file::FileKind::File)
            .await
            .unwrap();
        let event = a.delivery.lock().unwrap().message(id).unwrap().destinations[0]
            .event
            .clone();
        b.log.lock().unwrap().append_durable(event.clone()).unwrap();
        b.process_file_events(event.conversation_id);
        let sources = b.file_recovery_sources(file, 0).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].public, ap.public());
    }
}
