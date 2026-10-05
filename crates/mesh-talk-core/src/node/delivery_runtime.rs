//! Atomic DM acceptance and control receipt dispatch. Network hooks take owned
//! immutable snapshots; no delivery or ratchet guard crosses a transport await.
use super::delivery_store::{DeliveryDestination, ReceiptDelivery};
use super::delivery_store::{DeliveryStore, DeliveryTransaction, OutgoingDelivery};
use super::sentlog::SentEntry;
use super::*;
use crate::eventlog::{ConversationId, Event, EventId, LogError};

#[derive(Default)]
struct DeliveryDiagnosticThrottle {
    last_emitted: Option<std::time::Instant>,
}

impl DeliveryDiagnosticThrottle {
    fn admit(&mut self, now: std::time::Instant) -> bool {
        if self.last_emitted.is_some_and(|last| {
            now.saturating_duration_since(last) < std::time::Duration::from_secs(30)
        }) {
            return false;
        }
        self.last_emitted = Some(now);
        true
    }

    fn warn_if_due(&mut self) {
        if self.admit(std::time::Instant::now()) {
            log::warn!("automatic delivery local persistence deferred");
        }
    }
}

impl Node {
    pub(in crate::node) fn persist_account_adoption<T>(
        &self,
        account: &str,
        save: impl FnOnce() -> Result<T, NodeError>,
    ) -> Result<T, NodeError> {
        let mut store = self.delivery.lock().expect("delivery lock not poisoned");
        self.recover_delivery(&mut store).map_err(NodeError::Log)?;
        store
            .prepare_profile_adoption(account)
            .map_err(NodeError::Log)?;
        match save() {
            Ok(result) => {
                self.delivery_suspended
                    .store(true, std::sync::atomic::Ordering::Release);
                Ok(result)
            }
            Err(error) => {
                if store.cancel_profile_adoption().is_err() {
                    log::warn!("account adoption marker cleanup deferred");
                }
                Err(error)
            }
        }
    }
    pub(in crate::node) fn delivery_destination_allowed(
        &self,
        destination: &DeliveryDestination,
    ) -> bool {
        !self
            .delivery_suspended
            .load(std::sync::atomic::Ordering::Acquire)
            && self
                .historical_author(&destination.device.ed25519_pub)
                .is_some_and(|p| {
                    p.public() == destination.device
                        && (destination.account.is_none() || p.account_id() == destination.account)
                })
    }
    pub(in crate::node) fn accept_dm_event(
        &self,
        event: &Event,
        proof: &crate::discovery::Announce,
        store: &mut DeliveryStore,
    ) -> Option<ReceivedDm> {
        let peer = proof.public();
        let own = self.identity.public();
        if event.conversation_id == super::delivery_receipt::delivery_conversation_id(&own, &peer) {
            self.process_delivery_control(event, proof, store);
            return None;
        }
        if event.conversation_id != super::conversation::dm_conversation_id(&own, &peer)
            || event.author.ed25519_pub() != &peer.ed25519_pub
            || !event.verify_integrity()
            || !event.verify_signature()
            || event.ciphertext.len() > 256 * 1024
            || event.parents.len() > 1024
        {
            return None;
        }
        store
            .check_capacity(1, event.ciphertext.len() as u64)
            .ok()?;
        let (plain, prepared) = self
            .dm_ratchet
            .lock()
            .expect("ratchet lock not poisoned")
            .prepare_decrypt(&self.identity, &peer, &event.ciphertext)
            .ok()?;
        if plain.len() > 256 * 1024 {
            return None;
        }
        let account = self.account_id();
        let (conversation, from, body) = if plain.starts_with(b"MTDE1") {
            let env = DmEnvelope::decode(&plain)?;
            let valid_account = |id: &str| {
                id.len() == 32
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            };
            if !valid_account(&env.route.sender_account)
                || !valid_account(&env.route.recipient_account)
                || proof.account_id().as_deref() != Some(env.route.sender_account.as_str())
                || (env.route.sender_account != account && env.route.recipient_account != account)
            {
                return None;
            }
            (
                super::conversation::account_conversation_id(
                    &env.route.sender_account,
                    &env.route.recipient_account,
                ),
                env.route.sender_account,
                MessageBody::decode(&env.body),
            )
        } else {
            (
                event.conversation_id,
                peer.user_id(),
                MessageBody::decode(&plain),
            )
        };
        let received = ReceivedEntry {
            event_id: event.id,
            conversation,
            from,
            wall_clock: event.wall_clock,
            plaintext: plain,
        };
        let payload = super::delivery_receipt::ReceiptPayload::prepare(
            &own,
            &account,
            proof,
            event,
            &received,
            super::node::now_millis(),
        );
        let receipt = if let Some(payload) = payload {
            if store.has_receipt_for(conversation, event.id) {
                return None;
            }
            let wire = payload.seal(&self.identity).ok()?;
            let log = self.log.lock().expect("log lock not poisoned");
            let conv = payload.conversation();
            let (mut parents, lamport) = log.prepare(&conv);
            // The Ed-pair scope survives account adoption and local deletion.
            // Never make a fresh receipt depend on control ancestry that the
            // current profile cannot safely project to this exact account.
            let controls = self
                .delivery_control_ids
                .lock()
                .expect("control ids lock not poisoned");
            let mut eligible = std::collections::HashSet::new();
            for previous in log.events(&conv) {
                let typed = (previous.author.ed25519_pub() == &own.ed25519_pub
                    && controls
                        .get(&previous.id)
                        .is_some_and(|b| b.device == peer && b.account == proof.account_id()))
                    || super::delivery_receipt::open_receipt(
                        &self.identity,
                        &account,
                        proof,
                        previous,
                    )
                    .is_some();
                if typed && previous.parents.iter().all(|id| eligible.contains(id)) {
                    eligible.insert(previous.id);
                }
            }
            parents.retain(|id| eligible.contains(id));
            drop(controls);
            let author = crate::eventlog::Author::from_ed25519(own.ed25519_pub);
            let seq = log.version_vector(&conv).get(&author).copied().unwrap_or(0) + 1;
            let control = Event::new(
                &self.identity,
                conv,
                seq,
                parents,
                lamport,
                payload.confirmed_at(),
                crate::eventlog::EventKind::Message,
                wire,
            );
            log.sync().ok()?;
            let logical_id = DmEnvelope::decode(&received.plaintext)
                .map_or(event.id, |env| EventId::new(env.msg_id));
            Some(Box::new(ReceiptDelivery {
                logical_id,
                original_event_id: event.id,
                conversation,
                wall_clock: event.wall_clock,
                destination: DeliveryDestination {
                    device: peer.clone(),
                    account: proof.account_id(),
                    event: control,
                    receipt_eligible: false,
                },
            }))
        } else {
            None
        };
        self.log
            .lock()
            .expect("log lock not poisoned")
            .sync()
            .ok()?;
        store
            .begin(DeliveryTransaction::Incoming {
                sender: peer.clone(),
                original: Box::new(event.clone()),
                ratchet: prepared,
                received: Box::new(received),
                receipt,
            })
            .ok()?;
        if self.recover_delivery(store).is_err() {
            log::warn!("accepted incoming delivery awaits local recovery");
            return None;
        }
        self.delivery_notify.notify_one();
        Some(ReceivedDm {
            from: peer.user_id(),
            from_name: proof.name.clone(),
            text: body.text,
            reply_to: body.reply_to,
        })
    }
    pub(in crate::node) fn recover_delivery(
        &self,
        store: &mut DeliveryStore,
    ) -> Result<(), LogError> {
        if self
            .delivery_suspended
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(LogError::CorruptFile(
                "restart node after account adoption".into(),
            ));
        }
        store.validate_owner(&self.identity.public(), &self.account_id())?;
        store.sync_replacement()?;
        if store.pending_transactions().is_empty() {
            return Ok(());
        }
        let mut ratchet = self.dm_ratchet.lock().expect("ratchet lock not poisoned");
        let mut log = self.log.lock().expect("log lock not poisoned");
        let mut received = self.received.lock().expect("received lock not poisoned");
        let mut sent = self.sentlog.lock().expect("sent lock not poisoned");
        loop {
            if let Some(DeliveryTransaction::Incoming {
                receipt: Some(receipt),
                ..
            }) = store.pending_transactions().first()
            {
                self.delivery_control_ids
                    .lock()
                    .expect("control ids lock not poisoned")
                    .insert(
                        receipt.destination.event.id,
                        super::delivery_store::ControlBinding {
                            device: receipt.destination.device.clone(),
                            account: receipt.destination.account.clone(),
                        },
                    );
            }
            let incoming = store
                .pending_transactions()
                .first()
                .and_then(|tx| match tx {
                    DeliveryTransaction::Incoming { original, .. } => Some(original.id),
                    _ => None,
                });
            if store
                .recover_next(&mut ratchet, &mut log, &mut sent, &mut received)?
                .is_none()
            {
                break;
            }
            if let Some(id) = incoming {
                self.emitted
                    .lock()
                    .expect("emitted lock not poisoned")
                    .insert(id);
            }
        }
        Ok(())
    }

    /// Current profile's local durable acceptance / authenticated delivery state.
    pub fn delivery_status(&self, id: EventId) -> Option<DeliveryStatus> {
        self.delivery
            .lock()
            .expect("delivery lock not poisoned")
            .status(id)
    }

    /// Bounded account-conversation status snapshot, with no plaintext payloads.
    pub fn account_delivery_statuses(
        &self,
        peer: &str,
        ids: &[EventId],
    ) -> Vec<(EventId, DeliveryStatus)> {
        let store = self.delivery.lock().expect("delivery lock not poisoned");
        let conv = super::conversation::account_conversation_id(&self.account_id(), peer);
        ids.iter()
            .take(256)
            .filter_map(|id| store.status_in(conv, *id).map(|s| (*id, s)))
            .collect()
    }

    /// A wake hook for hosts; the accept loop also schedules periodic retries.
    pub async fn delivery_work_notified(&self) {
        self.delivery_notify.notified().await;
    }
    pub async fn delivery_status_notified(&self) {
        self.delivery_status_notify.notified().await;
    }

    pub(in crate::node) fn raw_sent_history(
        &self,
        conversation: ConversationId,
    ) -> Vec<(EventId, SentEntry)> {
        let store = self.delivery.lock().expect("delivery lock not poisoned");
        let log = self.log.lock().expect("log lock not poisoned");
        let author = crate::eventlog::Author::from_ed25519(self.identity.public().ed25519_pub);
        let mut ids: std::collections::HashMap<_, _> = log
            .events(&conversation)
            .into_iter()
            .filter(|e| e.kind == crate::eventlog::EventKind::Message && e.author == author)
            .map(|e| (e.seq, e.id))
            .collect();
        let mut entries = self
            .sentlog
            .lock()
            .expect("sent lock not poisoned")
            .entries(&conversation);
        for tx in store.pending_transactions() {
            if let DeliveryTransaction::Outgoing { message, sent, .. } = tx {
                if sent.conversation == conversation {
                    ids.insert(sent.seq, message.logical_id);
                    if !entries.iter().any(|e| e == sent) {
                        entries.push(sent.clone());
                    }
                }
            }
        }
        entries
            .into_iter()
            .map(|e| (ids.get(&e.seq).copied().unwrap_or(EventId::new([0; 32])), e))
            .collect()
    }

    pub(in crate::node) fn sent_entries_for_history(
        &self,
        conversation: ConversationId,
    ) -> Vec<SentEntry> {
        let store = self.delivery.lock().expect("delivery lock not poisoned");
        let mut entries = self
            .sentlog
            .lock()
            .expect("sent lock not poisoned")
            .entries(&conversation);
        for tx in store.pending_transactions() {
            if let DeliveryTransaction::Outgoing { sent, .. } = tx {
                if sent.conversation == conversation && !entries.iter().any(|e| e == sent) {
                    entries.push(sent.clone());
                }
            }
        }
        entries
    }

    pub(in crate::node) fn process_delivery_control(
        &self,
        event: &Event,
        proof: &crate::discovery::Announce,
        store: &mut DeliveryStore,
    ) {
        let Some(opened) =
            super::delivery_receipt::open_receipt(&self.identity, &self.account_id(), proof, event)
        else {
            return;
        };
        let Some(message) = self.immutable_delivery(store, opened.logical_id()) else {
            return;
        };
        let Some(authenticated) = opened.authenticate(&message) else {
            return;
        };
        // Persist the signed control evidence before exposing the durable status.
        if self
            .log
            .lock()
            .expect("log lock not poisoned")
            .append_durable(event.clone())
            .is_err()
        {
            return;
        }
        if store
            .mark_delivered_for(
                authenticated.logical_id(),
                authenticated.original_event_id(),
            )
            .is_ok()
        {
            self.delivery_status_notify.notify_waiters();
        }
    }

    fn immutable_delivery(&self, store: &DeliveryStore, id: EventId) -> Option<OutgoingDelivery> {
        if let Some(message) = store.message(id) {
            return Some(message.clone());
        }
        let completed = store.completed_work(id)?;
        let log = self.log.lock().expect("log lock not poisoned");
        let mut destinations = Vec::with_capacity(completed.remaining.len());
        for reference in &completed.remaining {
            let event = log.get(&reference.event_id)?;
            if event.author.ed25519_pub() != &self.identity.public().ed25519_pub
                || event.wall_clock != completed.wall_clock
                || event.conversation_id
                    != super::conversation::dm_conversation_id(
                        &self.identity.public(),
                        &reference.device,
                    )
                || !event.verify_integrity()
                || !event.verify_signature()
            {
                return None;
            }
            destinations.push(DeliveryDestination {
                device: reference.device.clone(),
                account: reference.account.clone(),
                event: event.clone(),
                receipt_eligible: reference.receipt_eligible,
            });
        }
        Some(OutgoingDelivery {
            logical_id: id,
            sender_account: self.account_id(),
            recipient_account: completed.recipient_account.clone(),
            conversation: completed.conversation,
            wall_clock: completed.wall_clock,
            destinations,
        })
    }

    /// Immediate best-effort transfer of an already accepted immutable intent.
    /// A transport success is never a delivery confirmation.
    pub async fn flush_delivery(&self, id: EventId) {
        let message = {
            let mut store = self.delivery.lock().expect("delivery lock not poisoned");
            if self.recover_delivery(&mut store).is_err() {
                return;
            }
            self.immutable_delivery(&store, id)
        };
        let Some(message) = message else {
            return;
        };
        for destination in message.destinations {
            if !self.delivery_destination_allowed(&destination) {
                continue;
            }
            let conv = destination.event.conversation_id;
            if let Some(peer) = self
                .routing_peer(&destination.device.user_id())
                .filter(|p| {
                    p.public == destination.device
                        && (destination.account.is_none() || p.account_id == destination.account)
                        && self.known_account_allowed(&p.public)
                })
            {
                self.transfer_delivery_destination(&peer, &destination, conv)
                    .await;
                self.emit_new_messages(conv);
                // The remote serve loop processes plaintext after sync replies;
                // a second round requests control evidence over its own scope.
                let control = super::delivery_receipt::delivery_conversation_id(
                    &self.identity.public(),
                    &peer.public,
                );
                self.transfer_delivery_destination(&peer, &destination, control)
                    .await;
                self.emit_new_messages(control);
            }
            self.replicate_delivery_destination(&destination).await;
        }
    }

    /// Bounded direct receipt transfer hook. The shared worker separately
    /// qualifies exact durable custody and retires queued work.
    pub async fn flush_delivery_receipts(&self, limit: usize) {
        let receipts = {
            let mut store = self.delivery.lock().expect("delivery lock not poisoned");
            if self.recover_delivery(&mut store).is_err() {
                return;
            }
            store.retry_receipts(limit)
        };
        for receipt in receipts {
            let destination = receipt.destination;
            if !self.delivery_destination_allowed(&destination) {
                continue;
            }
            if let Some(peer) = self
                .routing_peer(&destination.device.user_id())
                .filter(|p| {
                    p.public == destination.device
                        && (destination.account.is_none() || p.account_id == destination.account)
                        && self.known_account_allowed(&p.public)
                })
            {
                self.transfer_delivery_destination(
                    &peer,
                    &destination,
                    destination.event.conversation_id,
                )
                .await;
                self.emit_new_messages(destination.event.conversation_id);
            }
        }
    }

    pub(in crate::node) async fn transfer_delivery_destination(
        &self,
        peer: &crate::discovery::PeerRecord,
        destination: &DeliveryDestination,
        conversation: ConversationId,
    ) -> bool {
        let Ok(mut channel) = self.privacy_dial(peer.addr, &destination.device).await else {
            return false;
        };
        if !self.delivery_destination_allowed(destination)
            || channel.peer_announcement().is_some_and(|p| {
                destination.account.is_some() && p.account_id() != destination.account
            })
            || !self
                .delivery
                .lock()
                .expect("delivery lock not poisoned")
                .contains_work_event(destination.event.id)
        {
            return false;
        }
        let store = self.sync_store(channel.peer_identity());
        if super::session::request_round(&mut channel, &store, conversation)
            .await
            .is_err()
        {
            return false;
        }
        if conversation != destination.event.conversation_id {
            return false;
        }
        let held =
            super::session::request_durable_have(&mut channel, conversation, destination.event.id)
                .await
                .unwrap_or(false);
        held && self.delivery_destination_allowed(destination)
    }

    pub(in crate::node) async fn replicate_delivery_destination(
        &self,
        destination: &DeliveryDestination,
    ) -> bool {
        let po = {
            let roster = self.roster.lock().expect("roster lock not poisoned");
            super::postbox::elected_post_office(&roster)
        };
        let Some(po) = po else {
            return false;
        };
        if po.public == self.identity.public() || !self.relay_allowed(&po.public) {
            return false;
        }
        let Ok(mut channel) = self.privacy_dial(po.addr, &po.public).await else {
            return false;
        };
        if !self.delivery_destination_allowed(destination)
            || !self.relay_allowed(&po.public)
            || channel
                .peer_announcement()
                .is_some_and(|p| po.account_id.is_some() && p.account_id() != po.account_id)
            || !self
                .delivery
                .lock()
                .expect("delivery lock not poisoned")
                .contains_work_event(destination.event.id)
        {
            return false;
        }
        let store = self.sync_store(channel.peer_identity());
        if super::session::request_round(&mut channel, &store, destination.event.conversation_id)
            .await
            .is_err()
        {
            return false;
        }
        let held = super::session::request_durable_have(
            &mut channel,
            destination.event.conversation_id,
            destination.event.id,
        )
        .await
        .unwrap_or(false);
        held && self.delivery_destination_allowed(destination) && self.relay_allowed(&po.public)
    }

    /// Shared by desktop, CLI and SDK accept-loop hosts. The future owns all
    /// cursors and network work, so dropping it cancels scheduling and dials.
    pub(in crate::node) async fn run_delivery_loop(self: std::sync::Arc<Self>) {
        use std::time::Duration;
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut destination_cursor = None;
        let mut receipt_cursor = None;
        let mut peer_cursor = None;
        let mut diagnostic = DeliveryDiagnosticThrottle::default();
        loop {
            tokio::select! {
                _ = interval.tick() => {},
                _ = self.delivery_notify.notified() => {},
            }
            let node = self.clone();
            let snapshot = tokio::task::spawn_blocking(move || {
                let mut store = node.delivery.lock().expect("delivery lock not poisoned");
                if node.recover_delivery(&mut store).is_err() {
                    return None;
                }
                let next = store.next_destination(destination_cursor);
                let destination = next.and_then(|(id, reference)| {
                    let event = node
                        .log
                        .lock()
                        .expect("log lock not poisoned")
                        .get(&reference.event_id)?
                        .clone();
                    Some((
                        id,
                        DeliveryDestination {
                            device: reference.device,
                            account: reference.account,
                            event,
                            receipt_eligible: reference.receipt_eligible,
                        },
                    ))
                });
                let receipt = store
                    .retry_receipts_after(receipt_cursor, 1)
                    .into_iter()
                    .next()
                    .or_else(|| store.retry_receipts_after(None, 1).into_iter().next());
                Some((destination, receipt))
            })
            .await;
            let Ok(Some((destination, receipt))) = snapshot else {
                diagnostic.warn_if_due();
                continue;
            };
            if let Some((id, destination)) = destination {
                destination_cursor = Some(id);
                let live = self
                    .delivery
                    .lock()
                    .expect("delivery lock not poisoned")
                    .contains_destination(id, destination.event.id);
                if live && self.retry_one_destination(&destination).await {
                    let mut store = self.delivery.lock().expect("delivery lock not poisoned");
                    if store.completed_work(id).is_some()
                        && store.retire_destination(id, destination.event.id).is_err()
                    {
                        diagnostic.warn_if_due();
                    }
                }
            }
            if let Some(receipt) = receipt {
                receipt_cursor = Some(receipt.destination.event.id);
                if self.retry_one_destination(&receipt.destination).await
                    && self
                        .delivery
                        .lock()
                        .expect("delivery lock not poisoned")
                        .finish_receipt(receipt.conversation, receipt.original_event_id)
                        .is_err()
                {
                    diagnostic.warn_if_due();
                }
            }
            // Historical identity authorizes a control pull even with its sender
            // offline. The cursor advances before any bounded network await.
            let peer = {
                let state = self
                    .privacy
                    .state
                    .read()
                    .expect("privacy lock not poisoned");
                if let Some(state) = state.as_ref() {
                    state.proofs.next_after(peer_cursor.as_deref())
                } else {
                    self.roster
                        .lock()
                        .expect("roster lock not poisoned")
                        .next_historical_after(peer_cursor.as_deref())
                }
            };
            if let Some(peer) = peer {
                peer_cursor = Some(peer.user_id.clone());
                if peer.post_office
                    || peer.public() == self.identity.public()
                    || !self.known_account_allowed(&peer.public())
                {
                    continue;
                }
                let conv = super::delivery_receipt::delivery_conversation_id(
                    &self.identity.public(),
                    &peer.public(),
                );
                let dm = super::conversation::dm_conversation_id(
                    &self.identity.public(),
                    &peer.public(),
                );
                let _ = tokio::time::timeout(
                    Duration::from_millis(400),
                    self.pull_delivery_scope(&peer, dm, true),
                )
                .await;
                self.emit_new_messages(dm);
                self.process_file_events(dm);
                let _ = tokio::time::timeout(
                    Duration::from_millis(400),
                    self.pull_delivery_scope(&peer, conv, true),
                )
                .await;
                let _ = tokio::time::timeout(
                    Duration::from_millis(400),
                    self.pull_delivery_scope(&peer, conv, false),
                )
                .await;
                self.emit_new_messages(conv);
            }
        }
    }

    async fn pull_delivery_scope(
        &self,
        proof: &crate::discovery::Announce,
        conv: ConversationId,
        relay: bool,
    ) {
        let current = || {
            self.historical_author(&proof.ed25519_pub).is_some_and(|p| {
                p.public() == proof.public() && p.account_id() == proof.account_id()
            }) && !self
                .delivery_suspended
                .load(std::sync::atomic::Ordering::Acquire)
        };
        if !current() {
            return;
        }
        let peer = if relay {
            let roster = self.roster.lock().expect("roster lock not poisoned");
            super::postbox::elected_post_office(&roster)
        } else {
            self.routing_peer(&proof.public().user_id())
        };
        let Some(peer) = peer else {
            return;
        };
        if relay && !self.relay_allowed(&peer.public) {
            return;
        }
        if !relay && (peer.public != proof.public() || peer.account_id != proof.account_id()) {
            return;
        }
        let Ok(mut channel) = self.privacy_dial(peer.addr, &peer.public).await else {
            return;
        };
        if !current()
            || (relay && !self.relay_allowed(&peer.public))
            || channel
                .peer_announcement()
                .is_some_and(|p| p.account_id() != peer.account_id)
        {
            return;
        }
        let store = self.pull_sync_store(channel.peer_identity());
        let _ = super::session::request_round(&mut channel, &store, conv).await;
    }

    async fn retry_one_destination(&self, destination: &DeliveryDestination) -> bool {
        use std::time::Duration;
        if !self.delivery_destination_allowed(destination) {
            return false;
        }
        let mut held = false;
        if let Some(peer) = self
            .routing_peer(&destination.device.user_id())
            .filter(|p| {
                p.public == destination.device
                    && p.account_id == destination.account
                    && self.known_account_allowed(&p.public)
            })
        {
            held = tokio::time::timeout(
                Duration::from_millis(400),
                self.transfer_delivery_destination(
                    &peer,
                    destination,
                    destination.event.conversation_id,
                ),
            )
            .await
            .unwrap_or(false);
            self.emit_new_messages(destination.event.conversation_id);
        }
        let relay_held = tokio::time::timeout(
            Duration::from_millis(400),
            self.replicate_delivery_destination(destination),
        )
        .await
        .unwrap_or(false);
        held || relay_held
    }
}

#[cfg(test)]
mod diagnostic_tests {
    #[test]
    fn delivery_local_failure_diagnostic_throttle_has_fixed_non_sliding_deadline() {
        use std::time::{Duration, Instant};
        let mut throttle = super::DeliveryDiagnosticThrottle::default();
        let start = Instant::now();
        assert!(throttle.admit(start));
        for millis in [0, 500, 1_000, 29_999] {
            assert!(!throttle.admit(start + Duration::from_millis(millis)));
        }
        assert!(throttle.admit(start + Duration::from_secs(30)));
        assert!(!throttle.admit(start + Duration::from_secs(30)));
        assert!(throttle.admit(start + Duration::from_secs(60)));
    }
}
