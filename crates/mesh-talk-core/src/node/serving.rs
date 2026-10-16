//! Outbound delivery (direct + post-office replication) and the inbound serve loop. Split out of node.rs (one `impl Node` block per domain).

use super::*;
use crate::discovery::roster::PeerRecord;
use crate::eventlog::event::{Author, ConversationId, Event, EventKind};
use crate::node::conversation::account_conversation_id;
use crate::node::session::{Served, SessionError, request_round, serve_one, serve_wire_bytes};
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
        let mut last_error = SessionError::UnexpectedMessage;
        for fresh in [false, true] {
            let mut channel = if fresh {
                self.privacy_dial(peer.addr, &peer.public).await
            } else {
                self.privacy_sync_dial(peer.addr, &peer.public).await
            }
            .map_err(SessionError::Transport)?;
            let store = self.sync_store(channel.peer_identity());
            match request_round(&mut channel, &store, conv).await {
                Ok(_) => {
                    self.recycle_sync_channel(channel);
                    return Ok(());
                }
                Err(error) => last_error = error,
            }
            // A peer can close immediately after the idle liveness check.
            // Signed idempotent reconciliation safely retries a fresh session.
        }
        Err(last_error)
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
            _ = self.accept_connections(listener) => {}
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
    pub async fn serve_connection(&self, mut channel: SecureChannel<TcpStream>) {
        if channel.pairing_only() {
            // A retired account proof can recover an interrupted authorized link,
            // but can never enter sync, account history, attachments or call IPC.
            if let Ok(Ok(first)) = tokio::time::timeout(SERVE_IDLE_TIMEOUT, channel.recv()).await
                && let Some(request) = PairingRequest::decode(&first)
            {
                self.serve_pairing(&mut channel, request).await;
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
        if let Some(req) = PairingRequest::decode(&first) {
            if !self.account_allowed(channel.peer_identity(), channel.peer_announcement()) {
                return;
            }
            self.serve_pairing(&mut channel, req).await;
            return;
        }
        if let Some(super::account_sync_wire::AccountHistoryFrame::Request {
            account_id,
            cursor,
            known_digest,
            wanted,
            ..
        }) = super::account_sync_wire::AccountHistoryFrame::decode(&first)
        {
            self.serve_account_history(&mut channel, &account_id, cursor, known_digest, wanted)
                .await;
            return;
        }
        if let Some(super::account_sync_wire::AccountHistoryFrame::AttachmentRequest {
            account_id,
            file_conv,
            ..
        }) = super::account_sync_wire::AccountHistoryFrame::decode(&first)
        {
            self.serve_account_attachment(
                &mut channel,
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
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let all_events: Vec<Event> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            log.events(&conv).into_iter().cloned().collect()
        };
        if !all_events
            .iter()
            .any(|e| e.kind == EventKind::FileManifest && e.author != self_author)
        {
            return;
        }
        // Is this a channel conversation? If so, open with its group key.
        let is_channel = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            book.state(&conv).is_some()
        };
        let content_gate = self.channel_content_gate(conv);
        let mut surfaced: Vec<ReceivedFile> = Vec::new();
        for event in all_events
            .into_iter()
            .filter(|event| event.kind == EventKind::FileManifest && event.author != self_author)
        {
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
                conv,
                is_channel,
                from: event.author.user_id(),
                name: manifest.name().to_string(),
                size: manifest.size(),
                mime: manifest.mime().to_string(),
                file_conv: manifest.file_conv(),
                media: crate::node::media_store::manifest_is_media(&manifest),
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
                    Some(acct) if acct != my_account => account_conversation_id(&my_account, &acct),
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
                continue;
            } // live journal recovery owns DM file publication
            self.remember_manifest_scope(manifest.file_conv(), conv, event.author, event.id);
            {
                let mut files = self.files.lock().expect("files mutex not poisoned");
                files.mark_emitted(event.id);
                files.record_event(event.id, event.author, manifest);
            }
            surfaced.push(received);
        }
        for rf in surfaced {
            // We have the manifest; if its chunks haven't all arrived, queue a direct pull
            // from peers (the sender's one-shot push may have raced our discovery, and on a
            // PO-less LAN nothing else fetches them). Cleared once the file completes.
            let fc = rf.file_conv;
            if self.attachment_consent_status(fc)
                == Some(super::attachment_consent::ReceiveDecision::Accepted)
                && matches!(self.file_progress(fc), Some(p) if p.done < p.total)
            {
                self.pending_files
                    .lock()
                    .expect("pending_files mutex not poisoned")
                    .insert(fc);
            } else {
                // All chunks already arrived with the manifest (the push landed): if this is
                // a media file, persist it into the durable store now (and prune its chunks).
                self.persist_media_if_complete(fc);
            }
            let _ = self.file_incoming.send(rf);
        }
    }

    /// Fetch chunks for any file we've surfaced a manifest for but don't fully hold yet,
    /// pulling DIRECTLY from connected peers. The post-office drain covers file convs only
    /// when a post office exists; on a plain peer-to-peer LAN the sender's one-shot push is
    /// otherwise the only delivery, so a push that missed (discovery race) would leave the
    /// recipient stuck at 0 chunks. Best-effort + idempotent: a complete file is dropped
    /// from the pending set; an unreachable peer is just skipped and retried next tick.
    pub async fn pull_pending_files(&self) {
        let pending: Vec<ConversationId> = {
            let set = self
                .pending_files
                .lock()
                .expect("pending_files mutex not poisoned");
            set.iter().copied().collect()
        };
        if pending.is_empty() {
            return;
        }
        let peers: Vec<PeerRecord> = {
            self.routing_peers()
                .into_iter()
                .filter(|p| !p.post_office)
                .collect()
        };
        for fc in pending {
            if self.attachment_consent_status(fc)
                != Some(super::attachment_consent::ReceiveDecision::Accepted)
            {
                self.pending_files
                    .lock()
                    .expect("pending_files mutex not poisoned")
                    .remove(&fc);
                continue;
            }
            // Already complete (e.g. the push did land, or a prior tick finished it)? Persist
            // media (no-op for attachments / already-stored) and drop it from the pending set.
            if matches!(self.file_progress(fc), Some(p) if p.done >= p.total) {
                if !self.persist_media_if_complete(fc) {
                    self.pending_files
                        .lock()
                        .expect("pending_files mutex not poisoned")
                        .remove(&fc);
                }
                continue;
            }
            for peer in &peers {
                if let Ok(mut channel) = self.privacy_dial(peer.addr, &peer.public).await {
                    let store = self.sync_store(channel.peer_identity());
                    let _ = request_round(&mut channel, &store, fc).await;
                }
                // Stop dialing more peers the moment this file is whole. Persist media into
                // the durable store (which also prunes its chunks + clears it from pending);
                // an attachment just drops out of the pending set.
                if matches!(self.file_progress(fc), Some(p) if p.done >= p.total) {
                    if !self.persist_media_if_complete(fc) {
                        self.pending_files
                            .lock()
                            .expect("pending_files mutex not poisoned")
                            .remove(&fc);
                    }
                    break;
                }
            }
        }
    }

    /// Run the channel book over `conv`'s events and stream any newly-decryptable
    /// channel messages. A no-op for DM conversations (no channel state).
    pub(in crate::node) fn process_channel(&self, conv: ConversationId) {
        let events: Vec<Event> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            log.events(&conv).into_iter().cloned().collect()
        };
        let refs: Vec<&Event> = events.iter().collect();
        let messages = {
            let mut book = self.channels.lock().expect("channels mutex not poisoned");
            book.process(&self.identity, conv, &refs)
        };
        let has_new_messages = !messages.is_empty();
        for msg in messages {
            // Persist the decrypted plaintext: the sender-key wire key is single-use,
            // so channel history is served from the received store, not re-opened.
            let _ = self
                .received
                .lock()
                .expect("received mutex not poisoned")
                .record(
                    conv,
                    msg.from.clone(),
                    msg.wall_clock,
                    &msg.wrapped,
                    msg.event_id,
                );
            let _ = self.channel_incoming.send(msg);
        }
        // ChannelBook returns only newly validated, decryptable messages. A
        // coalesced domain-specific wake advances bounded background gossip;
        // repeated processing and empty sync rounds do not create feedback.
        if has_new_messages {
            self.channel_recovery_notify.notify_one();
        }
        if self
            .log
            .lock()
            .expect("log mutex not poisoned")
            .events(&conv)
            .iter()
            .any(|event| event.kind == EventKind::Delete)
        {
            self.recalled_targets_channel(conv);
        }
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
        for po in post_offices {
            if !self.relay_receive_allowed(&po.public) {
                continue;
            }
            let Ok(mut channel) = self.privacy_sync_dial(po.addr, &po.public).await else {
                continue;
            };
            let mut reusable = true;
            // A pairwise mailbox can contain DMs, channel invitations,
            // controls, and file chunks unknown before this pull.
            for peer in self.historical_dm_peers() {
                if !self.verified_dm_pull_peer(&peer.public().user_id()) {
                    continue;
                }
                if !self.relay_receive_allowed(&po.public)
                    || self
                        .opaque_relay_round(&mut channel, &peer.public(), None)
                        .await
                        .is_err()
                {
                    reusable = false;
                    break;
                }
            }
            if reusable {
                self.recycle_sync_channel(channel);
            }
        }
    }

    /// Decrypt (via the Double Ratchet) and emit any not-yet-emitted, non-self
    /// `Message` events in `conv`. A message is marked emitted ONLY after it
    /// successfully decrypts and is recorded — so a transiently-undecryptable one
    /// (author not yet in the roster, or its ratchet key not yet derivable) is
    /// retried on a later sync rather than lost.
    pub(in crate::node) fn emit_new_messages(&self, conv: ConversationId) {
        let mut delivery = self.delivery.lock().expect("delivery lock not poisoned");
        if self.recover_delivery(&mut delivery).is_err() {
            log::warn!("DM processing awaits local delivery recovery");
            return;
        }
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let candidates: Vec<Event> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            let emitted = self.emitted.lock().expect("emitted mutex not poisoned");
            log.events(&conv)
                .into_iter()
                .filter(|e| {
                    e.kind == EventKind::Message
                        && e.author != self_author
                        && !emitted.contains(&e.id)
                })
                .cloned()
                .collect()
        };
        for event in candidates {
            let Some(proof) = self.historical_author(event.author.ed25519_pub()) else {
                continue;
            };
            let _ = self.accept_dm_event(&event, &proof, &mut delivery);
        }
        drop(delivery);
        // A recall may arrive before its target is viewed. Persist verified
        // tombstones while the old device and peer proofs are still available,
        // so an offline backup cannot resurrect the recalled plaintext.
        if self
            .log
            .lock()
            .expect("log mutex not poisoned")
            .events(&conv)
            .iter()
            .any(|event| event.kind == EventKind::Delete)
        {
            let my_account = self.account_id();
            let peers = self.historical_dm_peers();
            let matched: Vec<_> = peers
                .iter()
                .filter(|peer| {
                    super::conversation::dm_conversation_id(&self.identity.public(), &peer.public())
                        == conv
                })
                .filter_map(|peer| peer.account_id())
                .collect();
            for account in matched {
                if account == my_account {
                    for other in self.known_account_ids() {
                        self.recalled_targets_account(&other);
                    }
                } else {
                    self.recalled_targets_account(&account);
                }
            }
        }
    }
}
