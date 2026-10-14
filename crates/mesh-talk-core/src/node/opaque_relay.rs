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
struct OpaqueRoundStore {
    log: EventLog,
    seen: HashSet<EventId>,
}

impl SyncStore for OpaqueRoundStore {
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
        SyncStore::events_excluding(&self.log, conversation, have)
    }

    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        self.log.append(event)
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
        let mailbox = RelayMailbox::new(&self.identity, recipient, channel.peer_identity())
            .map_err(|e| super::session::SessionError::Serialization(e.to_string()))?;
        let seen = self
            .opaque_seen
            .lock()
            .expect("opaque seen lock not poisoned")
            .get(&mailbox.id())
            .cloned()
            .unwrap_or_default();
        let outer_store = Mutex::new(OpaqueRoundStore {
            log: EventLog::default(),
            seen,
        });
        let mut uploaded = Vec::new();
        if let Some(conv) = conversation {
            let events = {
                let store = self.sync_store(recipient);
                store
                    .lock()
                    .expect("projection lock not poisoned")
                    .events_excluding(&conv, &HashSet::new())
            };
            for event in events {
                let outer = mailbox
                    .wrap(&event)
                    .map_err(|e| super::session::SessionError::Serialization(e.to_string()))?;
                uploaded.push((event.id, outer.id));
                outer_store
                    .lock()
                    .expect("opaque store lock not poisoned")
                    .log
                    .append(outer)
                    .map_err(|e| super::session::SessionError::Serialization(e.to_string()))?;
            }
        }
        super::session::request_round(channel, &outer_store, mailbox.id()).await?;
        let wrappers: Vec<Event> = outer_store
            .lock()
            .expect("opaque store lock not poisoned")
            .log
            .events(&mailbox.id())
            .into_iter()
            .cloned()
            .collect();
        let mut inner: Vec<(EventId, Event)> = Vec::new();
        let mut processed = Vec::new();
        for outer in wrappers {
            match mailbox.open(&outer) {
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
                self.process_channel(conv);
                self.process_file_events(conv);
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
            cache.entry(mailbox.id()).or_default().extend(processed);
        }
        Ok(uploaded)
    }
}
