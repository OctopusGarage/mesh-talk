//! Recipient-encrypted event-log projection for an opaque relay mailbox.

use super::*;
use crate::eventlog::store::EventLog;
use crate::eventlog::sync::SyncStore;
use crate::eventlog::{AppendOutcome, ConversationId, Event, EventId, EventKind, LogError};
use crate::identity::device::PublicIdentity;
use crate::postoffice::opaque::RelayMailbox;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use tokio::io::{AsyncRead, AsyncWrite};

// IDs only, scoped to one mailbox operation. Exhaustion ends that operation;
// a later drain can retry after missing parents arrive or storage recovers.
const MAX_DEFERRED_EVENTS: usize = 4096;
// A retained operation keeps IDs and at most two MiB of deterministic wrappers.
// Missing data is wrapped one bounded page at a time, independent of file size.
const WRAPPER_CACHE_BYTES: usize = 2 * 1024 * 1024;
const WRAPPER_PAGE_BYTES: usize = crate::transport::MAX_PLAINTEXT * 4;

#[derive(Default)]
struct WrapperCache {
    entries: VecDeque<(EventId, Event)>,
    bytes: usize,
    #[cfg(test)]
    generated: usize,
}
impl WrapperCache {
    fn get(&self, id: EventId) -> Option<Event> {
        self.entries
            .iter()
            .find(|(inner, _)| *inner == id)
            .map(|(_, event)| event.clone())
    }
    fn insert(&mut self, inner: EventId, event: Event) {
        let bytes = event.ciphertext.len().saturating_add(512);
        if bytes > WRAPPER_CACHE_BYTES {
            return;
        }
        if self.entries.iter().any(|(id, _)| *id == inner) {
            return;
        }
        while self.bytes.saturating_add(bytes) > WRAPPER_CACHE_BYTES {
            if let Some((_, old)) = self.entries.pop_front() {
                self.bytes -= old.ciphertext.len().saturating_add(512);
            } else {
                break;
            }
        }
        self.bytes += bytes;
        self.entries.push_back((inner, event));
    }
    fn wrap(
        &mut self,
        mailbox: &RelayMailbox,
        inner: &Event,
    ) -> Result<Event, crate::postoffice::opaque::MailboxError> {
        if let Some(event) = self.get(inner.id) {
            return Ok(event);
        }
        #[cfg(test)]
        {
            self.generated += 1;
        }
        let event = mailbox.wrap(inner)?;
        self.insert(inner.id, event.clone());
        Ok(event)
    }
}

/// `seen` avoids pulling the same retained blobs every three seconds. The
/// signed inner event remains the durable truth; restored hints are checked
/// against retained inner events and can be discarded without losing delivery.
struct OpaqueRoundStore<'a> {
    node: &'a Node,
    relay: PublicIdentity,
    log: EventLog,
    seen: HashSet<EventId>,
    authorize_upload: &'a (dyn Fn() -> bool + Sync),
    mailbox: &'a RelayMailbox,
    recipient: &'a PublicIdentity,
    conversation: Option<ConversationId>,
    outgoing: &'a [(EventId, EventId)],
    controls: &'a HashMap<EventId, Event>,
    wrappers: &'a Mutex<WrapperCache>,
}

impl SyncStore for OpaqueRoundStore<'_> {
    fn requires_channel_v5(&self, _conversation: &ConversationId) -> Result<bool, LogError> {
        Ok(false)
    }
    // Both legacy and federated mailboxes wrap every inner kind as Message.
    // Capability checks must not materialize and seal an upload page merely
    // to rediscover that invariant on every fingerprint probe and sync step.
    fn requires_channel_v2(&self, _conversation: &ConversationId) -> Result<bool, LogError> {
        Ok(false)
    }

    fn requires_channel_v3(&self, _conversation: &ConversationId) -> Result<bool, LogError> {
        Ok(false)
    }

    fn requires_channel_v4(&self, _conversation: &ConversationId) -> Result<bool, LogError> {
        Ok(false)
    }

    fn event_ids(&self, conversation: &ConversationId) -> Result<Vec<EventId>, LogError> {
        let mut ids = self.seen.clone();
        ids.extend(SyncStore::event_ids(&self.log, conversation)?);
        ids.extend(self.outgoing.iter().map(|(_, outer)| *outer));
        Ok(ids.into_iter().collect())
    }

    fn events_excluding(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
    ) -> Result<Vec<Event>, LogError> {
        if !self.node.relay_send_allowed(&self.relay) || !(self.authorize_upload)() {
            return Ok(Vec::new());
        }
        let wanted: HashSet<_> = self
            .outgoing
            .iter()
            .filter(|(_, outer)| !have.contains(outer))
            .map(|(inner, _)| *inner)
            .collect();
        let mut source = if let Some(conv) = self.conversation {
            self.node
                .sync_store(self.recipient)
                .lock()
                .expect("projection lock not poisoned")
                .authorized_event_page(&conv, &wanted, WRAPPER_PAGE_BYTES)?
        } else {
            Vec::new()
        };
        source.extend(
            self.controls
                .values()
                .filter(|event| wanted.contains(&event.id))
                .cloned(),
        );
        let mut used = 0usize;
        let mut page = Vec::new();
        for inner in source {
            let Ok(outer) = self
                .wrappers
                .lock()
                .expect("wrapper cache lock not poisoned")
                .wrap(self.mailbox, &inner)
            else {
                continue;
            };
            let bytes = outer.ciphertext.len().saturating_add(512);
            if !page.is_empty() && used.saturating_add(bytes) > WRAPPER_PAGE_BYTES {
                break;
            }
            used += bytes;
            page.push(outer);
        }
        // Incoming wrappers already belong to the remote peer. Only locally
        // projected wrappers need to be returned as reciprocal uploads.
        let _ = conversation;
        Ok(page)
    }

    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        if !self.node.relay_receive_allowed(&self.relay) {
            // Round-local only: do not mark ignored incoming data durably seen.
            self.seen.insert(event.id);
            return Ok(AppendOutcome::Duplicate);
        }
        if self.outgoing.iter().any(|(_, outer)| *outer == event.id)
            && event.is_canonical()
            && event.verify_integrity()
            && event.verify_signature()
        {
            return Ok(AppendOutcome::Duplicate);
        }
        self.log.append(event)
    }
}

/// One mailbox projection retained only between complete protocol exchanges.
/// Cancellation requires discarding this state together with its channel.
/// Retains principal-bound IDs and a two-MiB wrapper cache. Outgoing pages are
/// authorized afresh without cloning full ciphertext history. Each deterministic
/// wrapper is generated for its ID and again only when its upload page misses
/// the bounded cache; ordinary streamed transfer therefore performs O(N) wraps.
pub(in crate::node) struct OpaqueMailboxProgress {
    mailbox: RelayMailbox,
    recipient: PublicIdentity,
    relay: PublicIdentity,
    identity_controls: Option<Vec<Event>>,
    consent_offers: Option<Vec<(ConversationId, EventId)>>,
    conversation: Option<ConversationId>,
    sync: super::session::SyncProgress,
    uploaded: Vec<(EventId, EventId)>,
    deferred_events: HashSet<EventId>,
    processed_events: HashMap<EventId, Option<EventId>>,
    wrapper_ids: HashMap<EventId, EventId>,
    wrappers: Mutex<WrapperCache>,
}

impl OpaqueMailboxProgress {
    pub(in crate::node) fn new(
        relay: &PublicIdentity,
        recipient: &PublicIdentity,
        conversation: Option<ConversationId>,
        mailbox: RelayMailbox,
    ) -> Self {
        Self {
            mailbox,
            recipient: recipient.clone(),
            relay: relay.clone(),
            identity_controls: None,
            consent_offers: None,
            conversation,
            sync: super::session::SyncProgress::default(),
            uploaded: Vec::new(),
            deferred_events: HashSet::new(),
            processed_events: HashMap::new(),
            wrapper_ids: HashMap::new(),
            wrappers: Mutex::new(WrapperCache::default()),
        }
    }

    pub(in crate::node) fn uploaded(&self) -> &[(EventId, EventId)] {
        &self.uploaded
    }

    fn defer_event(&mut self, outer: EventId) -> Result<(), super::session::SessionError> {
        if self.deferred_events.len() >= MAX_DEFERRED_EVENTS
            && !self.deferred_events.contains(&outer)
        {
            return Err(super::session::SessionError::NoProgress);
        }
        self.deferred_events.insert(outer);
        Ok(())
    }
}

impl Node {
    /// Reconcile one pairwise mailbox, uploading only events authorized for
    /// `recipient`. Every event returned by the relay is decrypted and passed
    /// through the ordinary local admission gate before it is surfaced.
    pub(in crate::node) async fn opaque_relay_round<IO>(
        &self,
        channel: &mut crate::transport::SecureChannel<IO>,
        recipient: &PublicIdentity,
        conversation: Option<ConversationId>,
    ) -> Result<Vec<(EventId, EventId)>, super::session::SessionError>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        if !self.relay_send_allowed(channel.peer_identity())
            && !self.relay_receive_allowed(channel.peer_identity())
        {
            return Ok(Vec::new());
        }
        let legacy = RelayMailbox::new(&self.identity, recipient, channel.peer_identity())
            .map_err(|e| super::session::SessionError::Serialization(e.to_string()))?;
        let mut uploaded = self
            .opaque_mailbox_round(channel, recipient, conversation, legacy)
            .await?;
        if self.bridge_directory_allowed() {
            let federated = RelayMailbox::federated(&self.identity, recipient)
                .map_err(|e| super::session::SessionError::Serialization(e.to_string()))?;
            uploaded.extend(
                self.opaque_mailbox_round(channel, recipient, conversation, federated)
                    .await?,
            );
        }
        Ok(uploaded)
    }

    async fn opaque_mailbox_round<IO>(
        &self,
        channel: &mut crate::transport::SecureChannel<IO>,
        recipient: &PublicIdentity,
        conversation: Option<ConversationId>,
        mailbox: RelayMailbox,
    ) -> Result<Vec<(EventId, EventId)>, super::session::SessionError>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        let mut progress =
            OpaqueMailboxProgress::new(channel.peer_identity(), recipient, conversation, mailbox);
        while !self.advance_opaque_mailbox(channel, &mut progress).await? {}
        Ok(progress.uploaded)
    }

    pub(in crate::node) async fn advance_opaque_mailbox<IO>(
        &self,
        channel: &mut crate::transport::SecureChannel<IO>,
        progress: &mut OpaqueMailboxProgress,
    ) -> Result<bool, super::session::SessionError>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        self.advance_opaque_mailbox_if(channel, progress, &|| true)
            .await
    }

    pub(in crate::node) async fn advance_opaque_mailbox_if<IO, F>(
        &self,
        channel: &mut crate::transport::SecureChannel<IO>,
        progress: &mut OpaqueMailboxProgress,
        authorize_upload: &F,
    ) -> Result<bool, super::session::SessionError>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
        F: Fn() -> bool + Sync,
    {
        if channel.peer_identity() != &progress.relay {
            return Err(super::session::SessionError::UnexpectedMessage);
        }
        let mut seen = self
            .opaque_receive_cache
            .lock()
            .expect("opaque cache lock not poisoned")
            .seen(
                progress.mailbox.id(),
                &self.log.lock().expect("log lock not poisoned"),
            )?;
        seen.extend(progress.deferred_events.iter().copied());
        // Optional hint persistence may fail independently of admitted history.
        // Keep only this active exchange's progress, under the existing HAVE
        // identifier budget, so a disabled cache cannot pin the same page.
        {
            let log = self.log.lock().expect("log lock not poisoned");
            for (outer, inner) in &progress.processed_events {
                if match inner {
                    Some(inner) => log.has(inner)?,
                    None => true,
                } {
                    seen.insert(*outer);
                }
            }
        }
        let mut controls = HashMap::new();
        let mut source_ids = Vec::new();
        if self.relay_send_allowed(&progress.relay) && authorize_upload() {
            if let Some(conv) = progress.conversation {
                source_ids = self
                    .sync_store(&progress.recipient)
                    .lock()
                    .expect("projection lock not poisoned")
                    .authorized_event_ids(&conv)?;
            }
            if progress.identity_controls.is_none() {
                progress.identity_controls =
                    Some(self.file_message_identity_controls_for(&progress.recipient, false)?);
            }
            if let Some(identities) = &progress.identity_controls {
                let guarded = self.sync_store(&progress.recipient);
                for bundle in identities {
                    let mut authorized = false;
                    for (file, manifest) in super::file_message_identity::bundle_references(bundle)
                    {
                        if guarded
                            .lock()
                            .expect("projection lock")
                            .file_message_identity(&file, &manifest)?
                            .is_some_and(|current| current.id == bundle.id)
                        {
                            authorized = true;
                            break;
                        }
                    }
                    if authorized {
                        controls.insert(bundle.id, bundle.clone());
                    }
                }
            }
            if progress.consent_offers.is_none() {
                progress.consent_offers = Some(self.file_consent_offers(&progress.recipient)?);
            }
            controls.extend(
                self.file_consent_controls(
                    &progress.recipient,
                    progress.consent_offers.as_deref().unwrap_or(&[]),
                    false,
                )?
                .into_iter()
                .map(|event| (event.id, event)),
            );
        }
        let requested: HashSet<_> = source_ids.iter().copied().collect();
        source_ids.extend(controls.keys().copied());
        if source_ids.len() > 200_000 {
            return Err(super::session::SessionError::NoProgress);
        }
        let mut outgoing = Vec::with_capacity(source_ids.len());
        for (index, inner) in source_ids.into_iter().enumerate() {
            if index.is_multiple_of(8) {
                tokio::task::yield_now().await;
            }
            let outer = if let Some(outer) = progress.wrapper_ids.get(&inner) {
                *outer
            } else {
                let event = match controls.get(&inner) {
                    Some(event) => Some(event.clone()),
                    None => self
                        .log
                        .lock()
                        .expect("log lock not poisoned")
                        .get(&inner)?,
                };
                let Some(event) = event else {
                    continue;
                };
                let wrapper = progress
                    .wrappers
                    .lock()
                    .expect("wrapper cache lock not poisoned")
                    .wrap(&progress.mailbox, &event)
                    .map_err(|error| {
                        super::session::SessionError::Serialization(error.to_string())
                    })?;
                progress.wrapper_ids.insert(inner, wrapper.id);
                wrapper.id
            };
            outgoing.push((inner, outer));
        }
        let current: HashSet<_> = outgoing.iter().map(|(inner, _)| *inner).collect();
        progress
            .wrapper_ids
            .retain(|inner, _| current.contains(inner));
        progress.uploaded = outgoing
            .iter()
            .copied()
            .filter(|(inner, _)| requested.contains(inner))
            .collect();
        let outer_store = Mutex::new(OpaqueRoundStore {
            node: self,
            relay: progress.relay.clone(),
            log: EventLog::default(),
            seen,
            authorize_upload,
            mailbox: &progress.mailbox,
            recipient: &progress.recipient,
            conversation: progress.conversation,
            outgoing: &outgoing,
            controls: &controls,
            wrappers: &progress.wrappers,
        });
        let complete = progress
            .sync
            .step(channel, &outer_store, progress.mailbox.id())
            .await?;
        let wrappers: Vec<Event> = outer_store
            .lock()
            .expect("opaque store lock not poisoned")
            .log
            .events(&progress.mailbox.id())
            .into_iter()
            .cloned()
            .collect();
        drop(outer_store);
        let mut inner: Vec<(EventId, Event)> = Vec::new();
        let mut processed = Vec::new();
        let mut retained = Vec::new();
        for outer in wrappers {
            match progress.mailbox.open(&outer) {
                Ok(event) => inner.push((outer.id, event)),
                Err(_) => processed.push(outer.id),
            }
        }
        // A file's manifest is in its DM conversation and its chunks are in
        // another conversation, so the DAG alone does not order them.
        inner.sort_by_key(|(_, event)| {
            (
                event.kind != EventKind::FileManifest,
                event.lamport,
                event.id,
            )
        });
        let store = self.sync_store(channel.peer_identity());
        for (outer_id, event) in inner {
            if !self.relay_receive_allowed(channel.peer_identity()) {
                break;
            }
            if super::file_message_identity::is_bundle(&event) {
                use super::file_message_identity::FileMessageIdentityImportError;
                if self
                    .delivery_suspended
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    break;
                }
                let source =
                    super::file_message_identity::bundle_accounts(&event).map(|(source, _)| source);
                let actor = self
                    .historical_author(&progress.recipient.ed25519_pub)
                    .filter(|proof| proof.public() == progress.recipient)
                    .and_then(|proof| proof.account_id());
                if source.is_none()
                    || source != actor
                    || !self.known_account_allowed(&progress.recipient)
                {
                    processed.push(outer_id);
                    continue;
                }
                match self.import_file_message_identity_checked(event) {
                    Ok(()) | Err(FileMessageIdentityImportError::Invalid(_)) => {
                        processed.push(outer_id)
                    }
                    Err(
                        FileMessageIdentityImportError::MissingProof
                        | FileMessageIdentityImportError::Persistence(_),
                    ) => {
                        progress.defer_event(outer_id)?;
                    }
                }
                continue;
            }
            match self.accept_file_consent_event(&event, &progress.recipient) {
                Ok(true) => {
                    processed.push(outer_id);
                    continue;
                }
                Err(error) => {
                    // Retry this control after local storage recovers, while
                    // allowing unrelated messages in the same mailbox through.
                    log::debug!("file consent persistence deferred: {error}");
                    // Do not re-download this failed control in every protocol
                    // step. This ID is deliberately absent from the node-wide seen cache:
                    // a fresh mailbox operation must retry its persistence.
                    progress.defer_event(outer_id)?;
                    continue;
                }
                Ok(false) => {}
            }
            if self
                .log
                .lock()
                .expect("log mutex not poisoned")
                .has(&event.id)?
            {
                processed.push(outer_id);
                retained.push((outer_id, event.id));
                continue;
            }
            let inner_id = event.id;
            let conv = event.conversation_id;
            let admission = store
                .lock()
                .expect("projection lock not poisoned")
                .ingest(event);
            if let Err(error) = admission {
                // Valid wrappers are only transport progress: missing parents,
                // receive policy or storage failure may reject their contents.
                // Recreating the round store must not make the same rejected
                // page appear newly applied forever. Defer only this operation,
                // so later drains retry after dependencies or policy recover.
                log::debug!("opaque event admission deferred: {error}");
                progress.defer_event(outer_id)?;
            } else {
                processed.push(outer_id);
                if self
                    .log
                    .lock()
                    .expect("log lock not poisoned")
                    .has(&inner_id)?
                {
                    retained.push((outer_id, inner_id));
                }
                self.emit_new_messages(conv);
                self.process_channel_reception(conv, "catch_up");
                self.process_file_events_reception(conv, "catch_up");
                self.process_profile_events(conv);
                self.persist_media_if_complete(conv);
            }
        }
        let retained_by_outer: HashMap<_, _> = retained.iter().copied().collect();
        for outer in &processed {
            if progress.processed_events.len() >= super::session::MAX_HAVE_IDS_TOTAL
                && !progress.processed_events.contains_key(outer)
            {
                return Err(super::session::SessionError::NoProgress);
            }
            progress
                .processed_events
                .insert(*outer, retained_by_outer.get(outer).copied());
        }
        if !retained.is_empty() {
            self.opaque_receive_cache
                .lock()
                .expect("opaque cache lock not poisoned")
                .remember(progress.mailbox.id(), &retained);
        }
        // Retained pairs stay dependent on exact main-log membership. Only
        // wrappers without a retained inner event need session-only progress.
        processed.retain(|outer| !retained_by_outer.contains_key(outer));
        if !processed.is_empty() {
            self.opaque_receive_cache
                .lock()
                .expect("opaque cache lock not poisoned")
                .remember_session(progress.mailbox.id(), &processed);
        }
        if !self.relay_send_allowed(channel.peer_identity()) {
            progress.uploaded.clear();
        }
        Ok(complete)
    }
}

#[cfg(test)]
mod permission_tests {
    #[test]
    fn bounded_wrapper_cache_reuses_wrappers_and_limits_ciphertext_memory() {
        let alice = crate::identity::device::DeviceIdentity::generate();
        let bob = crate::identity::device::DeviceIdentity::generate();
        let relay = crate::identity::device::DeviceIdentity::generate();
        let mailbox =
            crate::postoffice::opaque::RelayMailbox::new(&alice, &bob.public(), &relay.public())
                .unwrap();
        let conv = crate::eventlog::ConversationId::new([8; 32]);
        let events: Vec<_> = (0..80)
            .map(|seq| {
                crate::eventlog::Event::new(
                    &alice,
                    conv,
                    seq,
                    vec![],
                    seq,
                    0,
                    crate::eventlog::EventKind::Message,
                    vec![7; 32_000],
                )
            })
            .collect();
        let mut cache = super::WrapperCache::default();
        let ids: Vec<_> = events
            .iter()
            .map(|event| cache.wrap(&mailbox, event).unwrap().id)
            .collect();
        assert!(cache.bytes <= super::WRAPPER_CACHE_BYTES);
        // The initial metadata pass and one streamed upload must do at most
        // two wraps per event even when the projection exceeds the byte budget.
        for (event, expected) in events.iter().zip(ids) {
            let outer = cache.wrap(&mailbox, event).unwrap();
            assert_eq!(outer.id, expected);
            assert_eq!(cache.wrap(&mailbox, event).unwrap().id, expected);
            assert!(cache.bytes <= super::WRAPPER_CACHE_BYTES);
        }
        assert!(cache.generated <= events.len() * 2);
    }

    use super::*;
    use crate::identity::{account::Account, device::DeviceIdentity};
    use std::sync::Arc;

    #[tokio::test]
    async fn opaque_receive_restart_skips_payload_and_recovers_missing_inner() {
        struct ObservedRelay {
            log: EventLog,
            exported: std::cell::Cell<usize>,
        }
        impl SyncStore for ObservedRelay {
            fn requires_channel_v2(&self, _: &ConversationId) -> Result<bool, LogError> {
                Ok(false)
            }
            fn requires_channel_v3(&self, _: &ConversationId) -> Result<bool, LogError> {
                Ok(false)
            }
            fn requires_channel_v4(&self, _: &ConversationId) -> Result<bool, LogError> {
                Ok(false)
            }
            fn event_ids(&self, conv: &ConversationId) -> Result<Vec<EventId>, LogError> {
                SyncStore::event_ids(&self.log, conv)
            }
            fn events_excluding(
                &self,
                conv: &ConversationId,
                have: &HashSet<EventId>,
            ) -> Result<Vec<Event>, LogError> {
                self.log.events_excluding(conv, have)
            }
            fn events_excluding_bounded(
                &self,
                conv: &ConversationId,
                have: &HashSet<EventId>,
                bytes: usize,
                frames: usize,
            ) -> Result<Vec<Event>, LogError> {
                let events = self
                    .log
                    .events_excluding_bounded(conv, have, bytes, frames)?;
                self.exported.set(self.exported.get() + events.len());
                Ok(events)
            }
            fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
                self.log.append(event)
            }
        }
        for federated in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let identity = DeviceIdentity::generate();
            let keys = identity.secret_bytes();
            let account = Account::generate();
            let account_key = account.secret_bytes();
            let remote = DeviceIdentity::generate();
            let proof = crate::discovery::Announce::new_with_account(
                &remote,
                &Account::generate(),
                "Remote",
                47475,
            );
            let relay = DeviceIdentity::generate();
            let relay_public = relay.public();
            let mailbox = if federated {
                RelayMailbox::federated(&identity, &remote.public())
            } else {
                RelayMailbox::new(&identity, &remote.public(), &relay_public)
            }
            .unwrap();
            let conv = super::super::conversation::dm_conversation_id(
                &identity.public(),
                &remote.public(),
            );
            let inner = Event::new(
                &remote,
                conv,
                1,
                vec![],
                1,
                0,
                EventKind::Message,
                vec![7; 32000],
            );
            let outer = mailbox.wrap(&inner).unwrap();
            let mut relay_log = EventLog::default();
            relay_log.append(outer.clone()).unwrap();
            let relay_log = Arc::new(Mutex::new(ObservedRelay {
                log: relay_log,
                exported: std::cell::Cell::new(0),
            }));
            let newer = Event::new(
                &remote,
                conv,
                2,
                vec![inner.id],
                2,
                0,
                EventKind::Message,
                vec![8; 32000],
            );
            for pass in 0..6 {
                if pass == 2 {
                    relay_log
                        .lock()
                        .unwrap()
                        .log
                        .append(mailbox.wrap(&newer).unwrap())
                        .unwrap();
                }
                if pass == 4 {
                    // Corruption disables the optional cache, without preventing login or receive.
                    std::fs::write(root.path().join("events.log.opaque-receive"), b"corrupt")
                        .unwrap();
                }
                let (node, _, _) = super::super::test_support::node_with_files(
                    root.path(),
                    DeviceIdentity::from_secret_bytes(keys.0, keys.1),
                    Account::from_secret_bytes(account_key),
                    &proof,
                );
                node.cached_peer_snapshot_async().await.unwrap();
                if pass == 3 {
                    // Retained history may disappear independently of optional
                    // hints. Preserve the published source barrier and use the
                    // actual repository maintenance boundary.
                    node.log.lock().unwrap().drop_conversation(&conv).unwrap();
                }
                let before = relay_log.lock().unwrap().exported.get();
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let store = relay_log.clone();
                let relay_keys = relay.secret_bytes();
                let task = tokio::spawn(async move {
                    let (server, _) = listener.accept().await.unwrap();
                    let relay = DeviceIdentity::from_secret_bytes(relay_keys.0, relay_keys.1);
                    let mut channel = crate::transport::SecureChannel::accept(server, &relay)
                        .await
                        .unwrap();
                    while matches!(
                        super::super::session::serve_one(&mut channel, &store).await,
                        Ok(super::super::session::Served::Handled(_))
                    ) {}
                });
                let client = tokio::net::TcpStream::connect(address).await.unwrap();
                let mut channel = crate::transport::SecureChannel::connect(
                    client,
                    &node.identity,
                    Some(&relay_public),
                )
                .await
                .unwrap();
                let receiving_mailbox = if federated {
                    RelayMailbox::federated(&node.identity, &remote.public())
                } else {
                    RelayMailbox::new(&node.identity, &remote.public(), &relay_public)
                }
                .unwrap();
                tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    node.opaque_mailbox_round(
                        &mut channel,
                        &remote.public(),
                        None,
                        receiving_mailbox,
                    ),
                )
                .await
                .expect("optional hint failure cannot pin an active mailbox round")
                .unwrap();
                if pass >= 2 && !node.log.lock().unwrap().has(&newer.id).unwrap() {
                    // Outer IDs do not encode the inner DAG. If a child arrives
                    // on an earlier page, a fresh operation retries it after its
                    // parent has been admitted; rejected hints never persist.
                    let mailbox = if federated {
                        RelayMailbox::federated(&node.identity, &remote.public())
                    } else {
                        RelayMailbox::new(&node.identity, &remote.public(), &relay_public)
                    }
                    .unwrap();
                    node.opaque_mailbox_round(&mut channel, &remote.public(), None, mailbox)
                        .await
                        .unwrap();
                }
                drop(channel);
                task.await.unwrap();
                let after = relay_log.lock().unwrap().exported.get();
                assert!(node.log.lock().unwrap().has(&inner.id).unwrap());
                if pass >= 2 {
                    assert!(node.log.lock().unwrap().has(&newer.id).unwrap());
                }
                let transferred = after - before;
                if pass <= 2 {
                    assert_eq!(
                        transferred,
                        usize::from(pass != 1),
                        "exact wrapper count for pass {pass}, federated={federated}"
                    );
                } else {
                    assert!(
                        (2..=3).contains(&transferred),
                        "missing-inner/corrupt-cache fallback transfers both wrappers, retrying a child-first page if necessary"
                    );
                }
                if pass == 4 {
                    // Even unavailable optional SQL hints cannot pin receiving.
                    // The next open must fall back to active-exchange progress.
                    node.storage
                        .database()
                        .transaction(|c| {
                            c.execute_batch("DROP INDEX opaque_session_epochs")?;
                            Ok(())
                        })
                        .unwrap();
                }
            }
        }
    }

    #[test]
    fn opaque_capability_probe_does_not_wrap_inner_controls() {
        let root = tempfile::tempdir().unwrap();
        let remote = DeviceIdentity::generate();
        let proof = crate::discovery::Announce::new_with_account(
            &remote,
            &Account::generate(),
            "Remote",
            47475,
        );
        let (node, _, _) = super::super::test_support::node_with_files(
            root.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let relay = DeviceIdentity::generate().public();
        let mailbox = RelayMailbox::new(&node.identity, &remote.public(), &relay).unwrap();
        let inner = Event::new(
            &node.identity,
            ConversationId::new([8; 32]),
            1,
            vec![],
            1,
            0,
            EventKind::ChannelAccountAuthority,
            vec![7; 32_000],
        );
        let outer = mailbox.wrap(&inner).unwrap();
        assert_eq!(outer.kind, EventKind::Message);
        let controls = HashMap::from([(inner.id, inner.clone())]);
        let outgoing = [(inner.id, outer.id)];
        let wrappers = Mutex::new(WrapperCache::default());
        let store = OpaqueRoundStore {
            node: &node,
            relay,
            log: EventLog::default(),
            seen: HashSet::new(),
            authorize_upload: &|| true,
            mailbox: &mailbox,
            recipient: &remote.public(),
            conversation: None,
            outgoing: &outgoing,
            controls: &controls,
            wrappers: &wrappers,
        };
        assert!(!store.requires_channel_v2(&mailbox.id()).unwrap());
        assert!(!store.requires_channel_v3(&mailbox.id()).unwrap());
        assert!(!store.requires_channel_v4(&mailbox.id()).unwrap());
        assert!(!store.requires_channel_v5(&mailbox.id()).unwrap());
        assert_eq!(
            wrappers.lock().unwrap().generated,
            0,
            "opaque capability checks must not seal or clone upload pages"
        );
    }

    #[tokio::test]
    async fn rejected_inner_event_does_not_pin_relay_pages_and_is_retried_next_operation() {
        use crate::discovery::Announce;
        use tokio::net::{TcpListener, TcpStream};

        let root = tempfile::tempdir().unwrap();
        let remote = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = Announce::new_with_account(&remote, &account, "Remote", 47475);
        let (node, _, _) = super::super::test_support::node_with_files(
            root.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        node.cached_peer_snapshot_async().await.unwrap();
        let rejected = Event::new(
            &remote,
            ConversationId::new([42; 32]),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![1; 32_000],
        );
        let conv = super::super::conversation::dm_conversation_id(
            &node.identity.public(),
            &remote.public(),
        );
        let accepted = Event::new(
            &remote,
            conv,
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![2; 32_000],
        );
        // A valid opaque wrapper can contain an inner event which current
        // attachment policy rejects. Put it before the next page's admissible DM.
        let (relay, mailbox, rejected_outer, accepted_outer) = (0..64)
            .find_map(|_| {
                let relay = DeviceIdentity::generate();
                let mailbox =
                    RelayMailbox::new(&node.identity, &remote.public(), &relay.public()).unwrap();
                let first = mailbox.wrap(&rejected).unwrap();
                let second = mailbox.wrap(&accepted).unwrap();
                (first.id < second.id).then_some((relay, mailbox, first, second))
            })
            .expect("find a mailbox whose rejected event precedes the accepted event");
        assert!(
            rejected_outer.ciphertext.len() + accepted_outer.ciphertext.len()
                > crate::transport::MAX_PLAINTEXT
        );
        let rejected_id = rejected_outer.id;
        let relay_public = relay.public();
        let mut relay_store = EventLog::default();
        relay_store.append(rejected_outer).unwrap();
        relay_store.append(accepted_outer).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut channel = crate::transport::SecureChannel::accept(stream, &relay)
                .await
                .unwrap();
            let store = Mutex::new(relay_store);
            while matches!(
                super::super::session::serve_one(&mut channel, &store).await,
                Ok(super::super::session::Served::Handled(_))
            ) {}
        });
        let stream = TcpStream::connect(address).await.unwrap();
        let mut channel =
            crate::transport::SecureChannel::connect(stream, &node.identity, Some(&relay_public))
                .await
                .unwrap();
        let mut progress =
            OpaqueMailboxProgress::new(&relay_public, &remote.public(), None, mailbox);
        let completed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for _ in 0..6 {
                if node
                    .advance_opaque_mailbox(&mut channel, &mut progress)
                    .await
                    .unwrap()
                {
                    return true;
                }
            }
            false
        })
        .await
        .expect("bounded relay steps must not stall");
        assert!(
            completed,
            "a rejected inner event must not be downloaded every round"
        );
        assert!(node.log.lock().unwrap().has(&accepted.id).unwrap());
        assert!(!node.log.lock().unwrap().has(&rejected.id).unwrap());
        assert!(
            !node
                .opaque_receive_cache
                .lock()
                .unwrap()
                .seen(progress.mailbox.id(), &node.log.lock().unwrap())
                .unwrap()
                .contains(&rejected_id),
            "rejected data must remain retryable after policy or storage recovery"
        );
        // A fresh operation really receives and retries the rejected wrapper;
        // the old operation's transfer hint must not become durable admission.
        let mailbox = RelayMailbox::new(&node.identity, &remote.public(), &relay_public).unwrap();
        let mut next = OpaqueMailboxProgress::new(&relay_public, &remote.public(), None, mailbox);
        for _ in 0..2 {
            assert!(
                !node
                    .advance_opaque_mailbox(&mut channel, &mut next)
                    .await
                    .unwrap()
            );
        }
        assert!(next.deferred_events.contains(&rejected_id));
        drop(channel);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn relay_direction_matrix_preserves_pending_local_events() {
        for send in [false, true] {
            for receive in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let identity = DeviceIdentity::generate();
                let remote = DeviceIdentity::generate();
                let account = Account::generate();
                let (dm, _) = tokio::sync::mpsc::unbounded_channel();
                let (group, _) = tokio::sync::mpsc::unbounded_channel();
                let (files, _) = tokio::sync::mpsc::unbounded_channel();
                let node = Node::open(
                    identity,
                    Arc::new(Mutex::new(crate::discovery::roster::Roster::default())),
                    dm,
                    group,
                    files,
                    &root.path().join("messages"),
                    &root.path().join("sent"),
                    "pw",
                )
                .unwrap();
                let remote_proof = crate::discovery::Announce::new_with_account(
                    &remote, &account, "Remote", 47475,
                );
                node.roster.lock().unwrap().update(
                    &remote_proof,
                    "127.0.0.1".parse().unwrap(),
                    &node.user_id(),
                );
                node.cached_peer_snapshot_async().await.unwrap();
                let relay = DeviceIdentity::generate();
                let keys = relay.secret_bytes();
                let relay_public = relay.public();
                let mailbox =
                    RelayMailbox::new(&node.identity, &remote.public(), &relay_public).unwrap();
                let conv = super::super::conversation::dm_conversation_id(
                    &node.identity.public(),
                    &remote.public(),
                );
                let outgoing = Event::new(
                    &node.identity,
                    conv,
                    1,
                    vec![],
                    1,
                    0,
                    EventKind::Message,
                    b"outgoing".to_vec(),
                );
                node.log.lock().unwrap().append(outgoing.clone()).unwrap();
                let incoming_conv = conv;
                let incoming = Event::new(
                    &remote,
                    incoming_conv,
                    1,
                    vec![],
                    1,
                    0,
                    EventKind::Message,
                    b"incoming".to_vec(),
                );
                let incoming_outer = mailbox.wrap(&incoming).unwrap();
                let relay_store = Arc::new(Mutex::new(EventLog::default()));
                relay_store
                    .lock()
                    .unwrap()
                    .append(incoming_outer.clone())
                    .unwrap();
                node.set_post_office_permissions(send, receive);
                let (client_io, server_io) = tokio::io::duplex(128 * 1024);
                let server_store = relay_store.clone();
                let task = tokio::spawn(async move {
                    let id = DeviceIdentity::from_secret_bytes(keys.0, keys.1);
                    let mut channel = crate::transport::SecureChannel::accept(server_io, &id)
                        .await
                        .unwrap();
                    while matches!(
                        super::super::session::serve_one(&mut channel, &server_store).await,
                        Ok(super::super::session::Served::Handled(_))
                    ) {}
                });
                let mut channel = crate::transport::SecureChannel::connect(
                    client_io,
                    &node.identity,
                    Some(&relay_public),
                )
                .await
                .unwrap();
                node.opaque_relay_round(&mut channel, &remote.public(), Some(conv))
                    .await
                    .unwrap();
                assert_eq!(
                    relay_store
                        .lock()
                        .unwrap()
                        .has(&mailbox.wrap(&outgoing).unwrap().id),
                    send
                );
                assert!(
                    node.log.lock().unwrap().has(&outgoing.id).unwrap(),
                    "disabling upload retains accepted local work"
                );
                assert_eq!(
                    node.log.lock().unwrap().has(&incoming.id).unwrap(),
                    receive,
                    "receive permission gates durable inner-event admission"
                );
                assert_eq!(
                    node.opaque_receive_cache
                        .lock()
                        .unwrap()
                        .seen(mailbox.id(), &node.log.lock().unwrap())
                        .unwrap()
                        .contains(&incoming_outer.id),
                    receive
                );
                drop(channel);
                task.await.unwrap();
                node.set_post_office_enabled(false);
                assert!(!node.relay_send_allowed(&relay_public));
                assert!(!node.relay_receive_allowed(&relay_public));
            }
        }
    }
}
