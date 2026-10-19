//! Recipient-encrypted event-log projection for an opaque relay mailbox.

use super::*;
use crate::eventlog::store::EventLog;
use crate::eventlog::sync::SyncStore;
use crate::eventlog::{AppendOutcome, ConversationId, Event, EventId, EventKind, LogError};
use crate::identity::device::PublicIdentity;
use crate::postoffice::opaque::RelayMailbox;
use std::collections::HashSet;
use std::sync::Mutex;
use tokio::io::{AsyncRead, AsyncWrite};

/// `seen` avoids pulling the same retained blobs every three seconds. The
/// signed inner event remains the durable truth; the cache is only an in-memory
/// transfer hint and can be discarded without losing delivery.
struct OpaqueRoundStore<'a> {
    node: &'a Node,
    relay: PublicIdentity,
    log: EventLog,
    seen: HashSet<EventId>,
    authorize_upload: &'a (dyn Fn() -> bool + Sync),
}

impl SyncStore for OpaqueRoundStore<'_> {
    fn event_ids(&self, conversation: &ConversationId) -> Vec<EventId> {
        let mut ids = self.seen.clone();
        ids.extend(SyncStore::event_ids(&self.log, conversation));
        ids.into_iter().collect()
    }

    fn events_excluding(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
    ) -> Vec<Event> {
        if !self.node.relay_send_allowed(&self.relay) || !(self.authorize_upload)() {
            return Vec::new();
        }
        SyncStore::events_excluding(&self.log, conversation, have)
    }

    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        if !self.node.relay_receive_allowed(&self.relay) {
            // Round-local only: do not mark ignored incoming data durably seen.
            self.seen.insert(event.id);
            return Ok(AppendOutcome::Duplicate);
        }
        self.log.append(event)
    }
}

/// One mailbox projection retained only between complete protocol exchanges.
/// Cancellation requires discarding this state together with its channel.
/// Payload projections stay ephemeral: eight active files must not retain eight
/// full ciphertext logs. Rebuilding deterministic wrappers costs CPU per step,
/// but retains only identities, IDs, and protocol progress between exchanges.
pub(in crate::node) struct OpaqueMailboxProgress {
    mailbox: RelayMailbox,
    recipient: PublicIdentity,
    relay: PublicIdentity,
    conversation: Option<ConversationId>,
    sync: super::session::SyncProgress,
    uploaded: Vec<(EventId, EventId)>,
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
            conversation,
            sync: super::session::SyncProgress::default(),
            uploaded: Vec::new(),
        }
    }

    pub(in crate::node) fn uploaded(&self) -> &[(EventId, EventId)] {
        &self.uploaded
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
        let seen = self
            .opaque_seen
            .lock()
            .expect("opaque seen lock not poisoned")
            .get(&progress.mailbox.id())
            .cloned()
            .unwrap_or_default();
        let mut log = EventLog::default();
        let mut uploaded = Vec::new();
        let mut outgoing = Vec::new();
        if let Some(conv) = progress
            .conversation
            .filter(|_| self.relay_send_allowed(&progress.relay))
        {
            let events = {
                let store = self.sync_store(&progress.recipient);
                store
                    .lock()
                    .expect("projection lock not poisoned")
                    .events_excluding(&conv, &HashSet::new())
            };
            outgoing.extend(events.into_iter().map(|event| (event, true)));
        }
        if self.relay_send_allowed(&progress.relay) {
            match self.file_consent_events(&progress.recipient) {
                Ok(controls) => outgoing.extend(controls.into_iter().map(|event| (event, false))),
                Err(error) => log::debug!("file consent publication deferred: {error}"),
            }
        }
        for (event, requested) in outgoing {
            let outer = progress
                .mailbox
                .wrap(&event)
                .map_err(|e| super::session::SessionError::Serialization(e.to_string()))?;
            if requested {
                uploaded.push((event.id, outer.id));
            }
            log.append(outer)
                .map_err(|e| super::session::SessionError::Serialization(e.to_string()))?;
        }
        progress.uploaded = uploaded;
        let outer_store = Mutex::new(OpaqueRoundStore {
            node: self,
            relay: progress.relay.clone(),
            log,
            seen,
            authorize_upload,
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
        let mut inner: Vec<(EventId, Event)> = Vec::new();
        let mut processed = Vec::new();
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
            match self.accept_file_consent_event(&event, &progress.recipient) {
                Ok(true) => {
                    processed.push(outer_id);
                    continue;
                }
                Err(error) => {
                    // Retry this control after local storage recovers, while
                    // allowing unrelated messages in the same mailbox through.
                    log::debug!("file consent persistence deferred: {error}");
                    continue;
                }
                Ok(false) => {}
            }
            if self
                .log
                .lock()
                .expect("log mutex not poisoned")
                .has(&event.id)
            {
                processed.push(outer_id);
                continue;
            }
            let conv = event.conversation_id;
            if store
                .lock()
                .expect("projection lock not poisoned")
                .ingest(event)
                .is_ok()
            {
                processed.push(outer_id);
                self.emit_new_messages(conv);
                self.process_channel_reception(conv, "catch_up");
                self.process_file_events_reception(conv, "catch_up");
                self.process_profile_events(conv);
                self.persist_media_if_complete(conv);
            }
        }
        if !processed.is_empty() {
            let mut cache = self
                .opaque_seen
                .lock()
                .expect("opaque seen lock not poisoned");
            if cache.values().map(HashSet::len).sum::<usize>() > 250_000 {
                cache.clear();
            }
            cache
                .entry(progress.mailbox.id())
                .or_default()
                .extend(processed);
        }
        if !self.relay_send_allowed(channel.peer_identity()) {
            progress.uploaded.clear();
        }
        Ok(complete)
    }
}

#[cfg(test)]
mod permission_tests {
    use super::*;
    use crate::identity::{account::Account, device::DeviceIdentity};
    use std::sync::Arc;

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
                    node.log.lock().unwrap().has(&outgoing.id),
                    "disabling upload retains accepted local work"
                );
                assert_eq!(
                    node.log.lock().unwrap().has(&incoming.id),
                    receive,
                    "receive permission gates durable inner-event admission"
                );
                assert_eq!(
                    node.opaque_seen
                        .lock()
                        .unwrap()
                        .get(&mailbox.id())
                        .is_some_and(|seen| seen.contains(&incoming_outer.id)),
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
