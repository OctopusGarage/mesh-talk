//! Atomic DM acceptance and control receipt dispatch. Network hooks take owned
//! immutable snapshots; no delivery or ratchet guard crosses a transport await.
use super::delivery_store::{DeliveryDestination, ReceiptDelivery};
use super::delivery_store::{DeliveryStore, DeliveryTransaction, OutgoingDelivery};
use super::sentlog::SentEntry;
use super::session::SessionError;
use super::*;
use crate::eventlog::{ConversationId, Event, EventId, LogError};

// Owned by the delivery loop: cancellation drops every retained channel. Keep
// fewer sockets than the accept limit, and discard idle or unauthorized state.
const MAX_FILE_TRANSFERS: usize = 8;
const FILE_TRANSFER_IDLE: std::time::Duration = std::time::Duration::from_secs(10);

// Test-only timing: a dropped span records cancellation/early return, not success.
#[cfg(test)]
pub(super) struct TestTiming {
    id: u64,
    started: std::time::Instant,
    phase: &'static str,
    enabled: bool,
    finished: bool,
    records: Vec<(&'static str, u128, u128)>,
    ports: Option<(u16, u16)>,
    omitted: usize,
}

#[cfg(test)]
impl TestTiming {
    pub(super) fn new(phase: &'static str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let mut timing = Self {
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            started: std::time::Instant::now(),
            phase,
            enabled: std::env::var("MESH_TALK_TEST_TIMING").as_deref() == Ok("1"),
            finished: false,
            records: Vec::new(),
            ports: None,
            omitted: 0,
        };
        timing.mark(phase);
        timing
    }

    pub(super) fn mark(&mut self, phase: &'static str) {
        if self.enabled {
            static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
            if self.records.len() == 32 {
                self.records.remove(0);
                self.omitted += 1;
            }
            self.records.push((
                phase,
                self.started.elapsed().as_micros(),
                EPOCH
                    .get_or_init(std::time::Instant::now)
                    .elapsed()
                    .as_micros(),
            ));
        }
    }

    pub(super) fn phase(&mut self, phase: &'static str) {
        self.phase = phase;
        self.mark(phase);
    }

    pub(super) fn finish(&mut self) {
        self.mark("complete");
        self.finished = true;
    }

    pub(super) fn socket(&mut self, stream: &tokio::net::TcpStream) {
        if self.enabled {
            if let (Ok(local), Ok(remote)) = (stream.local_addr(), stream.peer_addr()) {
                self.ports = Some((local.port(), remote.port()));
            }
        }
    }
}

#[cfg(test)]
impl Drop for TestTiming {
    fn drop(&mut self) {
        if !self.finished {
            self.mark("dropped");
        }
        if self.enabled {
            use std::fmt::Write;
            let mut output = String::new();
            let _ = writeln!(
                output,
                "test-timing id={} finished={} last_phase={} omitted={} ports={:?}",
                self.id, self.finished, self.phase, self.omitted, self.ports
            );
            for (phase, elapsed, tick) in &self.records {
                let _ = writeln!(
                    output,
                    "test-timing id={} phase={phase} elapsed_us={elapsed} tick_us={tick}",
                    self.id
                );
            }
            // One stderr lock per bounded span, never one per protocol phase.
            eprint!("{output}");
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum FilePhase {
    Dial,
    Manifest,
    ManifestCustody,
    InitialCustody,
    Chunks,
    FinalCustody,
}

pub(in crate::node) struct FileTransfer {
    file: super::delivery_store::FileCard,
    destination: super::delivery_store::FileDestination,
    channel: Option<crate::transport::SecureChannel<tokio::net::TcpStream>>,
    phase: FilePhase,
    sync: super::session::SyncProgress,
    legacy: bool,
    last_used: std::time::Instant,
}

impl FileTransfer {
    fn new(
        file: super::delivery_store::FileCard,
        destination: super::delivery_store::FileDestination,
    ) -> Self {
        Self {
            file,
            destination,
            channel: None,
            phase: FilePhase::Dial,
            sync: super::session::SyncProgress::default(),
            legacy: false,
            last_used: std::time::Instant::now(),
        }
    }
}

type FileKey = (EventId, EventId);
type FileWork = (
    super::delivery_store::FileCard,
    super::delivery_store::FileDestination,
);

/// The worker owns file scheduling state across wakeups. A scan takes only a
/// cursor and a capacity snapshot, so no socket or mutable cache crosses the
/// blocking journal read.
#[derive(Default)]
struct FileDeliveryScheduler {
    pending_cursor: Option<FileKey>,
    active_cursor: Option<FileKey>,
    transfers: std::collections::HashMap<FileKey, FileTransfer>,
}

#[derive(Clone, Copy)]
struct FileScan {
    cursor: Option<FileKey>,
    slots: usize,
}

impl FileScan {
    fn read(self, store: &DeliveryStore) -> (Vec<FileWork>, Option<FileKey>) {
        let mut files = Vec::with_capacity(self.slots);
        let mut cursor = self.cursor;
        for _ in 0..self.slots {
            let Some((file, destination)) = store.next_file_destination(cursor) else {
                break;
            };
            let key = (file.id, destination.binding.event_id);
            if files.iter().any(|(previous, reference): &FileWork| {
                (previous.id, reference.binding.event_id) == key
            }) {
                break;
            }
            cursor = Some(key);
            files.push((file, destination));
        }
        (files, cursor)
    }
}

impl FileDeliveryScheduler {
    fn scan(&mut self, node: &Node) -> FileScan {
        node.prune_file_transfers(&mut self.transfers);
        FileScan {
            cursor: self.pending_cursor,
            slots: MAX_FILE_TRANSFERS - self.transfers.len(),
        }
    }

    fn admit(&mut self, node: &Node, files: Vec<FileWork>, cursor: Option<FileKey>) {
        self.pending_cursor = cursor;
        node.prune_file_transfers(&mut self.transfers);
        for (file, destination) in files {
            let key = (file.id, destination.binding.event_id);
            if self.transfers.len() < MAX_FILE_TRANSFERS
                && node.file_destination_current(&file, &destination)
            {
                self.transfers
                    .entry(key)
                    .or_insert_with(|| FileTransfer::new(file, destination));
            }
        }
    }

    async fn advance(&mut self, node: &Node, diagnostic: &mut DeliveryDiagnosticThrottle) {
        // Bound each pass to eight complete exchanges, round-robin across
        // active slots. A failed attempt drops retained state and never self-wakes.
        let mut progress = std::collections::HashSet::new();
        for _ in 0..MAX_FILE_TRANSFERS {
            let mut keys = self.transfers.keys().copied().collect::<Vec<_>>();
            keys.sort_unstable();
            let Some(key) = keys
                .iter()
                .copied()
                .find(|key| self.active_cursor.is_none_or(|cursor| *key > cursor))
                .or_else(|| keys.first().copied())
            else {
                break;
            };
            self.active_cursor = Some(key);
            let transfer = self.transfers.get(&key).unwrap();
            let file = transfer.file.clone();
            let destination = transfer.destination.clone();
            if node
                .retry_file_step(&file, &destination, &mut self.transfers)
                .await
                && node
                    .delivery
                    .lock()
                    .expect("delivery lock not poisoned")
                    .retire_file_destination(file.id, destination.binding.event_id)
                    .is_err()
            {
                diagnostic.warn_if_due();
            }
            if self.transfers.contains_key(&key) {
                progress.insert(key);
            }
        }
        if progress.iter().any(|key| self.transfers.contains_key(key)) {
            #[cfg(test)]
            if self.transfers.is_empty() {
                node.empty_file_cache_self_wakes
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            node.delivery_notify.notify_one();
        }
    }
}

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
    /// Authorize one immutable outgoing journal entry, then attempt local replay.
    /// Once the journal accepts it, replay failure must not become a retryable send.
    pub(in crate::node) fn accept_outgoing_transaction(
        &self,
        store: &mut DeliveryStore,
        transaction: DeliveryTransaction,
        authorize: &mut impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<bool, NodeError> {
        let mut transaction = Some(transaction);
        authorize(&mut || {
            store
                .begin(transaction.take().expect("accept is single-use"))
                .map(|_| ())
                .map_err(NodeError::Log)
        })?;
        let recovered = self.recover_delivery(store).is_ok();
        self.delivery_notify.notify_one();
        Ok(recovered)
    }

    pub(in crate::node) fn accept_manifest_event(
        &self,
        event: &Event,
        proof: &crate::discovery::Announce,
        received: ReceivedEntry,
    ) -> Result<(), LogError> {
        let mut store = self.delivery.lock().expect("delivery lock not poisoned");
        self.recover_delivery(&mut store)?;
        let own = self.identity.public();
        let peer = proof.public();
        if !proof.verify()
            || event.author.ed25519_pub() != &peer.ed25519_pub
            || event.conversation_id != super::conversation::dm_conversation_id(&own, &peer)
        {
            return Err(LogError::CorruptFile("invalid manifest device pair".into()));
        }
        let manifest = super::files::validated_manifest(&received.plaintext)
            .ok_or_else(|| LogError::CorruptFile("invalid file manifest".into()))?;
        if store.manifest_event_erased(event.id)
            || manifest.file_conv() == event.conversation_id
            || manifest.file_conv() == received.conversation
            || self
                .files
                .lock()
                .expect("files lock not poisoned")
                .manifest(&manifest.file_conv())
                .is_some_and(|existing| !super::files::same_file_transfer(existing, &manifest))
        {
            return Err(LogError::CorruptFile(
                "conflicting immutable file manifest".into(),
            ));
        }
        {
            let log = self.log.lock().expect("log lock not poisoned");
            let mut chunks = log.events(&manifest.file_conv());
            chunks.sort_by_key(|chunk| chunk.seq);
            let mut previous = None;
            if chunks.iter().enumerate().any(|(index, chunk)| {
                let malformed_chain = chunk.seq != index as u64 + 1
                    || match previous {
                        None => !chunk.parents.is_empty(),
                        Some(id) => chunk.parents != [id],
                    };
                previous = Some(chunk.id);
                malformed_chain
                    || chunk.kind != crate::eventlog::EventKind::Message
                    || chunk.author != event.author
                    || chunk.seq == 0
                    || chunk.seq > u64::from(manifest.chunk_count())
                    || !chunk.verify_integrity()
                    || !chunk.verify_signature()
                    || crate::file::open_chunk_for(
                        &manifest,
                        (chunk.seq - 1) as u32,
                        &chunk.ciphertext,
                    )
                    .is_err()
            }) {
                return Err(LogError::CorruptFile(
                    "conflicting file chunk conversation".into(),
                ));
            }
        }
        let payload = super::delivery_receipt::ReceiptPayload::prepare(
            &own,
            &self.account_id(),
            proof,
            event,
            &received,
            super::node::now_millis(),
        );
        let receipt = if let Some(payload) = payload {
            if store.has_receipt_for(received.conversation, event.id) {
                return Ok(());
            }
            let log = self.log.lock().expect("log lock not poisoned");
            let conv = payload.conversation();
            let (_, lamport) = log.prepare(&conv);
            let author = crate::eventlog::Author::from_ed25519(own.ed25519_pub);
            let seq = log.version_vector(&conv).get(&author).copied().unwrap_or(0) + 1;
            let wire = payload
                .seal(&self.identity)
                .map_err(|_| LogError::CorruptFile("manifest receipt sealing failed".into()))?;
            let control = Event::new(
                &self.identity,
                conv,
                seq,
                vec![],
                lamport,
                payload.confirmed_at(),
                crate::eventlog::EventKind::Message,
                wire,
            );
            Some(Box::new(ReceiptDelivery {
                logical_id: event.id,
                original_event_id: event.id,
                conversation: received.conversation,
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
        self.log.lock().expect("log lock not poisoned").sync()?;
        let file = super::delivery_store::FileCard::incoming(
            event,
            &received,
            &manifest,
            super::delivery_store::FileCompletionBinding {
                source: peer.clone(),
                certificate: proof.account_cert.clone(),
                owner_account: self.account_id(),
            },
        );
        store.begin(DeliveryTransaction::IncomingManifest {
            sender: peer,
            original: Box::new(event.clone()),
            received: Box::new(received),
            receipt,
            file,
        })?;
        self.recover_delivery(&mut store)?;
        self.delivery_notify.notify_one();
        Ok(())
    }
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
    ) -> Option<()> {
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
        let (conversation, from, _body) = if plain.starts_with(b"MTDE1") {
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
        Some(())
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
        loop {
            let notification = store
                .pending_transactions()
                .first()
                .and_then(|tx| match tx {
                    DeliveryTransaction::Incoming {
                        sender,
                        original,
                        received,
                        ..
                    } => Some((
                        sender.clone(),
                        original.conversation_id,
                        (**received).clone(),
                        false,
                        true,
                    )),
                    DeliveryTransaction::IncomingManifest {
                        sender,
                        original,
                        received,
                        ..
                    } => Some((
                        sender.clone(),
                        original.conversation_id,
                        (**received).clone(),
                        true,
                        true,
                    )),
                    DeliveryTransaction::OutgoingManifest { received, .. } => Some((
                        self.identity.public(),
                        received.conversation,
                        (**received).clone(),
                        true,
                        false,
                    )),
                    _ => None,
                });
            let transaction_id = store.pending_transactions().first().map(|tx| tx.id());
            if let Some(
                DeliveryTransaction::Incoming {
                    receipt: Some(receipt),
                    ..
                }
                | DeliveryTransaction::IncomingManifest {
                    receipt: Some(receipt),
                    ..
                },
            ) = store.pending_transactions().first()
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
            let installed = {
                let mut ratchet = self.dm_ratchet.lock().expect("ratchet lock not poisoned");
                let mut log = self.log.lock().expect("log lock not poisoned");
                let mut received = self.received.lock().expect("received lock not poisoned");
                let mut sent = self.sentlog.lock().expect("sent lock not poisoned");
                let mut files = self.received_files.lock().expect("files lock not poisoned");
                store.recover_next_with_files(
                    &mut ratchet,
                    &mut log,
                    &mut sent,
                    &mut received,
                    Some(&mut files),
                )
            };
            let removed = transaction_id.is_some_and(|id| {
                store
                    .pending_transactions()
                    .first()
                    .is_none_or(|tx| tx.id() != id)
            });
            if removed {
                if let Some((sender, original_conv, entry, file, incoming)) = notification {
                    if file {
                        if let Some(manifest) = crate::file::decode_manifest(&entry.plaintext) {
                            let fresh = {
                                let mut book = self.files.lock().expect("files lock not poisoned");
                                let fresh = !book.is_emitted(&entry.event_id);
                                book.mark_emitted(entry.event_id);
                                book.record_event(entry.event_id, manifest.clone());
                                fresh
                            };
                            self.remember_manifest_scope(
                                manifest.file_conv(),
                                original_conv,
                                crate::eventlog::Author::from_ed25519(sender.ed25519_pub),
                                entry.event_id,
                            );
                            if !incoming {
                                self.install_file_card_scopes_for(store, Some(entry.event_id));
                            }
                            if incoming && fresh {
                                self.pending_files
                                    .lock()
                                    .expect("pending files lock not poisoned")
                                    .insert(manifest.file_conv());
                                let _ = self.file_incoming.send(ReceivedFile {
                                    conv: original_conv,
                                    from: sender.user_id(),
                                    name: manifest.name().to_owned(),
                                    size: manifest.size(),
                                    mime: manifest.mime().to_owned(),
                                    file_conv: manifest.file_conv(),
                                    media: super::media_store::manifest_is_media(&manifest),
                                });
                            }
                        }
                    } else if self
                        .emitted
                        .lock()
                        .expect("emitted lock not poisoned")
                        .insert(entry.event_id)
                    {
                        let body = DmEnvelope::decode(&entry.plaintext).map_or_else(
                            || MessageBody::decode(&entry.plaintext),
                            |env| MessageBody::decode(&env.body),
                        );
                        let from_name = self
                            .historical_author(&sender.ed25519_pub)
                            .map_or_else(|| sender.user_id(), |proof| proof.name);
                        let _ = self.incoming.send(ReceivedDm {
                            from: sender.user_id(),
                            from_name,
                            text: body.text,
                            reply_to: body.reply_to,
                        });
                    }
                }
            }
            if installed?.is_none() {
                break;
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
        // Own-device synchronization has no external recipient delivery claim.
        if peer == self.account_id() {
            return Vec::new();
        }
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
        let canonical = store
            .logical_for_original(opened.original_event_id())
            .unwrap_or_else(|| opened.logical_id());
        let Some(message) = self.immutable_delivery(store, canonical) else {
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
        let mut file_scheduler = FileDeliveryScheduler::default();
        let mut diagnostic = DeliveryDiagnosticThrottle::default();
        loop {
            tokio::select! {
                _ = interval.tick() => {},
                _ = self.delivery_notify.notified() => {},
            }
            let file_scan = file_scheduler.scan(&self);
            let node = self.clone();
            let Some(work) = self.runtime_work.admit() else {
                return;
            };
            let snapshot = tokio::task::spawn_blocking(move || {
                let _work = work;
                #[cfg(test)]
                if let Some(hook) = node.delivery_snapshot_hook.lock().unwrap().take() {
                    hook();
                }
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
                let (files, cursor) = file_scan.read(&store);
                Some((destination, receipt, files, cursor))
            })
            .await;
            let Ok(Some((destination, receipt, files, cursor))) = snapshot else {
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
            file_scheduler.admit(&self, files, cursor);
            file_scheduler.advance(&self, &mut diagnostic).await;
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

    pub(in crate::node) fn install_file_card_scopes(&self, store: &DeliveryStore) {
        self.install_file_card_scopes_for(store, None);
    }

    fn install_file_card_scopes_for(&self, store: &DeliveryStore, current: Option<EventId>) {
        let cards = if let Some(id) = current {
            store.file_card(id).into_iter().collect::<Vec<_>>()
        } else {
            store.file_cards().collect::<Vec<_>>()
        };
        let scopes = {
            let rows = self
                .received_files
                .lock()
                .expect("file rows lock not poisoned");
            let log = self.log.lock().expect("log lock not poisoned");
            cards
                .into_iter()
                .filter(|file| {
                    rows.entry(file.id).is_some_and(|entry| {
                        entry.conversation == file.conversation
                            && entry.from == self.user_id()
                            && super::files::eligible_manifest_row(
                                entry,
                                log.get(&entry.event_id),
                                None,
                            )
                            .is_some_and(|manifest| {
                                manifest.file_conv() == file.file_conversation
                                    && manifest.chunk_count() == file.chunk_count
                            })
                    })
                })
                .flat_map(|file| {
                    file.destinations.iter().filter_map(|destination| {
                        let event = log.get(&destination.binding.event_id)?;
                        (event.kind == crate::eventlog::EventKind::FileManifest
                            && event.author.ed25519_pub() == &self.identity.public().ed25519_pub
                            && event.conversation_id
                                == super::conversation::dm_conversation_id(
                                    &self.identity.public(),
                                    &destination.binding.device,
                                )
                            && event.verify_signature()
                            && event.verify_integrity())
                        .then_some((
                            file.file_conversation,
                            event.conversation_id,
                            event.author,
                            event.id,
                        ))
                    })
                })
                .collect::<Vec<_>>()
        };
        for (file, parent, author, event) in scopes {
            self.remember_manifest_scope(file, parent, author, event);
        }
    }

    async fn retry_file_destination(
        &self,
        file: &super::delivery_store::FileCard,
        destination: &super::delivery_store::FileDestination,
    ) -> bool {
        self.retry_file_destination_inner(file, destination, true)
            .await
    }

    fn file_destination_current(
        &self,
        file: &super::delivery_store::FileCard,
        destination: &super::delivery_store::FileDestination,
    ) -> bool {
        let binding = &destination.binding;
        self.delivery
            .lock()
            .expect("delivery lock not poisoned")
            .contains_file_destination(file.id, binding.event_id, &binding.device, &binding.account)
            && !self
                .delivery_suspended
                .load(std::sync::atomic::Ordering::Acquire)
            && self
                .historical_author(&binding.device.ed25519_pub)
                .is_some_and(|proof| {
                    proof.public() == binding.device && proof.account_id() == binding.account
                })
            && self.known_account_allowed(&binding.device)
    }

    pub(in crate::node) async fn retry_file_step(
        &self,
        file: &super::delivery_store::FileCard,
        destination: &super::delivery_store::FileDestination,
        transfers: &mut std::collections::HashMap<(EventId, EventId), FileTransfer>,
    ) -> bool {
        self.prune_file_transfers(transfers);
        let key = (file.id, destination.binding.event_id);
        if !self.file_destination_current(file, destination) {
            transfers.remove(&key);
            return false;
        }
        if !transfers.contains_key(&key) {
            if transfers.len() == MAX_FILE_TRANSFERS {
                // Existing destinations retain their progress; new destinations
                // still get the original bounded best-effort attempt this turn.
                return self.retry_file_destination(file, destination).await;
            }
            transfers.insert(key, FileTransfer::new(file.clone(), destination.clone()));
        }
        #[cfg(test)]
        self.file_transfer_peak
            .fetch_max(transfers.len(), std::sync::atomic::Ordering::SeqCst);
        let transfer = transfers.get_mut(&key).unwrap();
        transfer.last_used = std::time::Instant::now();
        #[cfg(test)]
        let mut timing = TestTiming::new(match transfer.phase {
            FilePhase::Dial => "file-dial",
            FilePhase::Manifest => "file-manifest-exchange",
            FilePhase::ManifestCustody => "file-manifest-custody",
            FilePhase::InitialCustody => "file-initial-custody",
            FilePhase::Chunks => "file-chunk-exchange",
            FilePhase::FinalCustody => "file-final-custody",
        });
        let outcome = tokio::time::timeout(
            std::time::Duration::from_millis(400),
            self.advance_file_transfer(transfer),
        )
        .await;
        #[cfg(test)]
        let phase = transfer.phase;
        #[cfg(test)]
        {
            timing.mark(match &outcome {
                Ok(Ok(true)) => "file-retirable",
                Ok(Ok(false)) => "file-clean-boundary",
                Ok(Err(_)) => "file-error",
                Err(_) => "file-400ms-cancelled",
            });
            timing.finish();
        }
        match outcome {
            Ok(Ok(true)) => {
                transfers.remove(&key);
                true
            }
            Ok(Ok(false)) => false,
            _ => {
                #[cfg(test)]
                eprintln!(
                    "file-step failure stage={phase:?} timeout={}",
                    outcome.is_err()
                );
                // A cancelled read/write may have consumed part of a frame.
                // Close the socket and restart from the remote's durable Have.
                transfers.remove(&key);
                self.retry_file_destination_inner(file, destination, false)
                    .await
            }
        }
    }

    fn prune_file_transfers(
        &self,
        transfers: &mut std::collections::HashMap<(EventId, EventId), FileTransfer>,
    ) {
        transfers.retain(|_, transfer| {
            transfer.last_used.elapsed() < FILE_TRANSFER_IDLE
                && self.file_destination_current(&transfer.file, &transfer.destination)
                && transfer
                    .channel
                    .as_ref()
                    .is_none_or(|channel| channel.io_admitted())
        });
    }

    async fn advance_file_transfer(
        &self,
        transfer: &mut FileTransfer,
    ) -> Result<bool, SessionError> {
        let file = &transfer.file;
        let destination = &transfer.destination;
        let binding = &destination.binding;
        if !self.file_destination_current(file, destination) {
            return Err(SessionError::UnexpectedMessage);
        }
        if matches!(transfer.phase, FilePhase::Dial) {
            let peer = self
                .routing_peer(&binding.device.user_id())
                .filter(|peer| peer.public == binding.device && peer.account_id == binding.account)
                .ok_or(SessionError::UnexpectedMessage)?;
            let channel = self.privacy_dial(peer.addr, &peer.public).await?;
            if !self.file_destination_current(file, destination)
                || channel
                    .peer_announcement()
                    .is_some_and(|proof| proof.account_id() != binding.account)
            {
                return Err(SessionError::UnexpectedMessage);
            }
            transfer.channel = Some(channel);
            transfer.phase = FilePhase::Manifest;
            return Ok(false);
        }
        let channel = transfer
            .channel
            .as_mut()
            .ok_or(SessionError::UnexpectedMessage)?;
        if !channel.io_admitted() || channel.peer_identity() != &binding.device {
            return Err(SessionError::UnexpectedMessage);
        }
        let store = self.sync_store(&binding.device);
        let final_chunk = file.final_chunk.ok_or(SessionError::UnexpectedMessage)?;
        match transfer.phase {
            FilePhase::Manifest => {
                let conversation = super::conversation::dm_conversation_id(
                    &self.identity.public(),
                    &binding.device,
                );
                if transfer.sync.step(channel, &store, conversation).await? {
                    transfer.sync = super::session::SyncProgress::default();
                    transfer.phase = if transfer.legacy {
                        FilePhase::Chunks
                    } else {
                        FilePhase::ManifestCustody
                    };
                }
            }
            FilePhase::ManifestCustody => {
                let conversation = super::conversation::dm_conversation_id(
                    &self.identity.public(),
                    &binding.device,
                );
                if !transfer.legacy {
                    match super::session::request_durable_have(
                        channel,
                        conversation,
                        binding.event_id,
                    )
                    .await
                    {
                        Ok(true) if self.file_destination_current(file, destination) => {
                            let mut delivery =
                                self.delivery.lock().expect("delivery lock not poisoned");
                            if delivery.completed_work(file.id).is_some() {
                                delivery
                                    .retire_destination(file.id, binding.event_id)
                                    .map_err(|error| {
                                        SessionError::Serialization(error.to_string())
                                    })?;
                            }
                        }
                        Ok(_) => {}
                        Err(_) => {
                            transfer.channel = None;
                            transfer.legacy = true;
                            transfer.sync = super::session::SyncProgress::default();
                            transfer.phase = FilePhase::Dial;
                            return Ok(false);
                        }
                    }
                }
                transfer.phase = if transfer.legacy {
                    FilePhase::Chunks
                } else {
                    FilePhase::InitialCustody
                };
            }
            FilePhase::InitialCustody => {
                if super::session::request_durable_have(
                    channel,
                    file.file_conversation,
                    final_chunk,
                )
                .await?
                {
                    return Ok(self.file_destination_current(file, destination));
                }
                transfer.phase = FilePhase::Chunks;
            }
            FilePhase::Chunks => {
                if transfer
                    .sync
                    .step(channel, &store, file.file_conversation)
                    .await?
                {
                    transfer.phase = FilePhase::FinalCustody;
                }
            }
            FilePhase::FinalCustody => {
                if super::session::request_durable_have(
                    channel,
                    file.file_conversation,
                    final_chunk,
                )
                .await?
                {
                    return Ok(self.file_destination_current(file, destination));
                }
                return Err(SessionError::NoProgress);
            }
            FilePhase::Dial => unreachable!(),
        }
        Ok(false)
    }

    async fn retry_file_destination_inner(
        &self,
        file: &super::delivery_store::FileCard,
        destination: &super::delivery_store::FileDestination,
        direct: bool,
    ) -> bool {
        let binding = &destination.binding;
        let current = || {
            self.delivery
                .lock()
                .expect("delivery lock not poisoned")
                .contains_file_destination(
                    file.id,
                    binding.event_id,
                    &binding.device,
                    &binding.account,
                )
                && !self
                    .delivery_suspended
                    .load(std::sync::atomic::Ordering::Acquire)
                && self
                    .historical_author(&binding.device.ed25519_pub)
                    .is_some_and(|proof| {
                        proof.public() == binding.device && proof.account_id() == binding.account
                    })
                && self.known_account_allowed(&binding.device)
        };
        if !current() {
            return false;
        }
        let manifest_conv =
            super::conversation::dm_conversation_id(&self.identity.public(), &binding.device);
        for relay in [false, true].into_iter().filter(|relay| *relay || direct) {
            let peer = if relay {
                let roster = self.roster.lock().expect("roster lock not poisoned");
                super::postbox::elected_post_office(&roster)
            } else {
                self.routing_peer(&binding.device.user_id())
            };
            let Some(peer) = peer else {
                continue;
            };
            if (relay && !self.relay_allowed(&peer.public))
                || (!relay && (peer.public != binding.device || peer.account_id != binding.account))
            {
                continue;
            }
            #[cfg(test)]
            let phase = std::sync::atomic::AtomicU8::new(0);
            let outcome = tokio::time::timeout(std::time::Duration::from_millis(400), async {
                let Ok(mut channel) = self.privacy_dial(peer.addr, &peer.public).await else {
                    return false;
                };
                #[cfg(test)]
                phase.store(1, std::sync::atomic::Ordering::Relaxed);
                if !current()
                    || (relay && !self.relay_allowed(&peer.public))
                    || channel
                        .peer_announcement()
                        .is_some_and(|proof| proof.account_id() != peer.account_id)
                {
                    return false;
                }
                let store = self.sync_store(channel.peer_identity());
                // Publish the manifest first so private recipients/relays can
                // durably reconstruct chunk scope before any chunk disclosure.
                if super::session::request_round(&mut channel, &store, manifest_conv)
                    .await
                    .is_err()
                {
                    return false;
                }
                if !current() || (relay && !self.relay_allowed(&peer.public)) {
                    return false;
                }
                let Some(final_chunk) = file.final_chunk else {
                    return false;
                };
                // A target may have verified/saved the file and reclaimed chunks
                // while its early card receipt was already in flight.
                if !relay {
                    #[cfg(test)]
                    phase.store(2, std::sync::atomic::Ordering::Relaxed);
                    match super::session::request_durable_have(
                        &mut channel,
                        file.file_conversation,
                        final_chunk,
                    )
                    .await
                    {
                        Ok(true) => return current(),
                        Ok(false) => {}
                        Err(_) => {
                            #[cfg(test)]
                            phase.store(3, std::sync::atomic::Ordering::Relaxed);
                            // Legacy endpoints may close on the optional probe.
                            // Reconnect and preserve manifest-first ordinary transfer.
                            let Ok(reconnected) = self.privacy_dial(peer.addr, &peer.public).await
                            else {
                                return false;
                            };
                            channel = reconnected;
                            if !current()
                                || channel
                                    .peer_announcement()
                                    .is_some_and(|proof| proof.account_id() != peer.account_id)
                            {
                                return false;
                            }
                            if super::session::request_round(&mut channel, &store, manifest_conv)
                                .await
                                .is_err()
                            {
                                return false;
                            }
                        }
                    }
                }
                #[cfg(test)]
                phase.store(4, std::sync::atomic::Ordering::Relaxed);
                if super::session::request_round(&mut channel, &store, file.file_conversation)
                    .await
                    .is_err()
                {
                    return false;
                }
                #[cfg(test)]
                phase.store(5, std::sync::atomic::Ordering::Relaxed);
                let held = super::session::request_durable_have(
                    &mut channel,
                    file.file_conversation,
                    final_chunk,
                )
                .await
                .unwrap_or(false);
                held && current() && (!relay || self.relay_allowed(&peer.public))
            })
            .await;
            #[cfg(test)]
            if !matches!(outcome, Ok(true)) {
                let stage = [
                    "dial",
                    "manifest",
                    "initial-custody",
                    "legacy-reconnect",
                    "chunks",
                    "final-custody",
                ][phase.load(std::sync::atomic::Ordering::Relaxed) as usize];
                eprintln!(
                    "file-attempt failure stage={stage} timeout={} relay={relay}",
                    outcome.is_err()
                );
            }
            let held = outcome.unwrap_or(false);
            if held && !relay {
                return true;
            }
        }
        false
    }

    pub(in crate::node) async fn flush_file_delivery(&self, id: EventId) {
        let file = {
            let mut store = self.delivery.lock().expect("delivery lock not poisoned");
            if self.recover_delivery(&mut store).is_err() {
                return;
            }
            store.file_card(id).cloned()
        };
        let Some(file) = file else {
            return;
        };
        for destination in &file.destinations {
            if destination.active && self.retry_file_destination(&file, destination).await {
                let _ = self
                    .delivery
                    .lock()
                    .expect("delivery lock not poisoned")
                    .retire_file_destination(file.id, destination.binding.event_id);
            }
        }
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
