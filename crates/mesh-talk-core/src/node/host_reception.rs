//! Demand materialization from the existing transcript/receipt owners. Hosts
//! retain cursor metadata; no payload is queued behind a stalled consumer.
use super::reception_effects::{ArrivalKind, ReceptionKind};
pub use super::reception_effects::{ReceptionConsumer, ReceptionCursor, ReceptionLimit};
use super::{
    DmEnvelope, MessageBody, Node, NodeError, ReceivedChannelMessage, ReceivedDm, ReceivedFile,
};

pub enum HostReception {
    DirectMessage(ReceivedDm),
    ChannelMessage(ReceivedChannelMessage),
    File(ReceivedFile),
}
pub struct HostReceptionPage {
    pub after: ReceptionCursor,
    pub next: ReceptionCursor,
    pub through: ReceptionCursor,
    pub complete: bool,
    pub receipts: Vec<HostReception>,
}
pub struct CurrentProfilePage {
    pub profiles: Vec<super::ReceivedProfile>,
    pub after: Option<String>,
    pub complete: bool,
    pub revision: u64,
}
impl Default for ReceptionLimit {
    fn default() -> Self {
        Self {
            rows: 64,
            body_bytes: super::reception_effects::MAX_RECEPTION_BODY_BYTES,
            serialized_bytes: super::reception_effects::MAX_RECEPTION_SERIALIZED_BYTES,
        }
    }
}
impl Node {
    pub fn current_profile_revision(&self) -> u64 {
        self.account_profiles.revision()
    }
    pub fn current_profiles_page(
        &self,
        revision: Option<u64>,
        after: Option<&str>,
    ) -> Result<CurrentProfilePage, NodeError> {
        let page = self.account_profiles.page(revision, after)?;
        Ok(CurrentProfilePage {
            profiles: page
                .rows
                .into_iter()
                .map(|(account_id, avatar)| super::ReceivedProfile { account_id, avatar })
                .collect(),
            after: page.after,
            complete: page.complete,
            revision: page.revision,
        })
    }
    pub fn reception_head(&self) -> Result<ReceptionCursor, NodeError> {
        Ok(self.reception_effects.head()?)
    }
    /// Fixed host roles resume their durable ACK, or enroll at the oldest
    /// available prefix. Enrolling another role never retires existing work.
    pub fn reception_resume(
        &self,
        consumer: ReceptionConsumer,
    ) -> Result<ReceptionCursor, NodeError> {
        Ok(self
            .reception_effects
            .register(consumer, self.reception_effects.oldest()?)?)
    }
    pub fn registered_reception(
        &self,
        consumer: ReceptionConsumer,
    ) -> Result<Option<ReceptionCursor>, NodeError> {
        Ok(self.reception_effects.registered(consumer)?)
    }
    pub fn acknowledge_reception(
        &self,
        consumer: ReceptionConsumer,
        after: ReceptionCursor,
        next: ReceptionCursor,
    ) -> Result<(), NodeError> {
        let current = self.reception_effects.register(consumer, after)?;
        if current != next {
            self.reception_effects.acknowledge(consumer, after, next)?;
        }
        self.reception_effects.retire(128)?;
        Ok(())
    }
    pub fn reception_page(
        &self,
        after: ReceptionCursor,
        through: Option<ReceptionCursor>,
        limit: ReceptionLimit,
    ) -> Result<HostReceptionPage, NodeError> {
        let through = through.unwrap_or(self.reception_effects.head()?);
        let page = self
            .reception_effects
            .page(after, through, limit, self.expiry_now())?;
        let mut receipts = Vec::with_capacity(page.rows.len());
        for fact in page.rows {
            let metadata = fact.metadata;
            if fact.source.kind == ReceptionKind::File {
                if let Some(entry) = fact.entry
                    && let Some(file) = self.received_file_for_reception(&entry, &metadata)?
                {
                    receipts.push(HostReception::File(file));
                }
                continue;
            }
            if fact.source.kind == ReceptionKind::DirectMessage {
                let Some(entry) = fact.entry else { continue };
                let envelope = DmEnvelope::decode(&entry.plaintext);
                let (id, account) = envelope
                    .as_ref()
                    .map_or((entry.event_id, None), |envelope| {
                        let peer = if envelope.route.sender_account == self.account_id() {
                            &envelope.route.recipient_account
                        } else {
                            &envelope.route.sender_account
                        };
                        (
                            crate::eventlog::EventId::new(envelope.msg_id),
                            Some(peer.as_str()),
                        )
                    });
                let inner = envelope
                    .as_ref()
                    .map_or(entry.plaintext.as_slice(), |envelope| {
                        envelope.body.as_slice()
                    });
                if self.recipient_expired(inner, id, entry.wall_clock)
                    || self.message_recalled_selected(
                        entry.conversation,
                        id,
                        account,
                        envelope.as_ref().map(|_| entry.from.as_str()),
                    )?
                {
                    continue;
                }
                let body = MessageBody::decode(inner);
                let from_name = self
                    .routing_peer(&metadata.from_device)
                    .map_or_else(|| metadata.from_device.clone(), |peer| peer.name);
                receipts.push(HostReception::DirectMessage(ReceivedDm {
                    message_id: envelope
                        .as_ref()
                        .map(|envelope| crate::eventlog::EventId::new(envelope.msg_id)),
                    from_account: envelope
                        .as_ref()
                        .map(|envelope| envelope.route.sender_account.clone()),
                    from: metadata.from_device,
                    from_name,
                    text: body.text,
                    protection: body.protection,
                    reply_to: body.reply_to,
                }));
                continue;
            }
            let (body, wall_clock) = match fact.entry {
                Some(entry) => {
                    if self.recipient_expired(&entry.plaintext, entry.event_id, entry.wall_clock)
                        || self.message_recalled_selected(
                            entry.conversation,
                            entry.event_id,
                            None,
                            None,
                        )?
                    {
                        continue;
                    }
                    (MessageBody::decode(&entry.plaintext), entry.wall_clock)
                }
                None => (MessageBody::new(Vec::new(), None), 0),
            };
            let channel_name = self
                .channels
                .lock()
                .expect("channels mutex not poisoned")
                .state(&fact.source.conversation)
                .map_or_else(String::new, |state| state.name().to_owned());
            receipts.push(HostReception::ChannelMessage(ReceivedChannelMessage {
                channel_id: fact.source.conversation,
                channel_name,
                from: metadata.from_device,
                text: body.text,
                protection: body.protection,
                reply_to: body.reply_to,
                event_id: fact.source.event_id,
                wall_clock,
                wrapped: Vec::new(),
                received_at: metadata.received_at,
                arrival_kind: match metadata.arrival_kind {
                    ArrivalKind::Live => "live",
                    ArrivalKind::CatchUp => "catch_up",
                    ArrivalKind::HistorySync => "history_sync",
                },
                own_device: metadata.own_device,
                batch_id: metadata.batch_id,
                batch_complete: metadata.batch_complete,
                batch_size: metadata.batch_size,
                reception_only: metadata.reception_only,
            }));
        }
        Ok(HostReceptionPage {
            after,
            next: page.next,
            through: page.through,
            complete: page.complete,
            receipts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        discovery::Roster,
        eventlog::EventId,
        identity::{account::Account, device::DeviceIdentity},
        node::{
            BoundedNodeOpenRequest,
            received_log::ReceivedEntry,
            received_store::ReceivedStore,
            reception_effects::{ArrivalKind, ReceptionMetadata},
        },
        storage::profile::ProfileStorageLease,
    };
    use std::sync::{Arc, Mutex};
    fn open(dir: &std::path::Path) -> Arc<Node> {
        Node::open_bounded_with_profile_lease(
            ProfileStorageLease::acquire(dir).unwrap(),
            BoundedNodeOpenRequest {
                identity: DeviceIdentity::generate(),
                account: Account::generate(),
                roster: Arc::new(Mutex::new(Roster::default())),
                log_path: &dir.join("messages.log"),
                sent_path: &dir.join("sent.log"),
                password: "pw",
            },
        )
        .unwrap()
    }
    fn receive(node: &Node, store: &mut ReceivedStore, n: u8) {
        let peer = "a".repeat(32);
        let entry = ReceivedEntry {
            event_id: EventId::new([n; 32]),
            conversation: super::super::conversation::account_conversation_id(
                &node.account_id(),
                &peer,
            ),
            from: peer.clone(),
            wall_clock: 100,
            plaintext: DmEnvelope::new(
                peer,
                node.account_id(),
                [n + 100; 32],
                MessageBody::new(format!("message-{n}").into_bytes(), None).encode(),
            )
            .encode(),
        };
        store
            .record_durable_with_reception(
                &entry,
                &ReceptionMetadata {
                    from_device: "peer-device".into(),
                    received_at: 200,
                    arrival_kind: ArrivalKind::Live,
                    own_device: false,
                    batch_id: None,
                    batch_complete: true,
                    batch_size: 1,
                    reception_only: false,
                },
            )
            .unwrap();
    }
    #[test]
    fn demand_pages_preserve_logical_identity_and_independent_autosave_ack() {
        let dir = tempfile::tempdir().unwrap();
        let node = open(dir.path());
        let mut received = ReceivedStore::from_profile(
            node.storage.clone(),
            &dir.path().join("received.log"),
            "pw",
        )
        .unwrap();
        let output = node.reception_resume(ReceptionConsumer::CliOutput).unwrap();
        let saves = node
            .reception_resume(ReceptionConsumer::CliAutosave)
            .unwrap();
        for n in 1..=3 {
            receive(&node, &mut received, n);
        }
        let first = node
            .reception_page(
                output,
                None,
                ReceptionLimit {
                    rows: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(first.receipts.len(), 1);
        assert!(!first.complete);
        let HostReception::DirectMessage(dm) = &first.receipts[0] else {
            panic!("expected DM")
        };
        assert_eq!(dm.message_id, Some(EventId::new([101; 32])));
        assert_eq!(dm.text, b"message-1");
        let next = first.next;
        node.acknowledge_reception(ReceptionConsumer::CliOutput, output, next)
            .unwrap();
        assert_eq!(
            node.reception_page(saves, None, ReceptionLimit::default())
                .unwrap()
                .receipts
                .len(),
            3
        );
        node.acknowledge_reception(ReceptionConsumer::CliAutosave, saves, next)
            .unwrap();
        // A lost ACK response is safely retried after metadata retirement.
        node.acknowledge_reception(ReceptionConsumer::CliOutput, output, next)
            .unwrap();
        let rest = node
            .reception_page(next, Some(first.through), ReceptionLimit::default())
            .unwrap();
        assert_eq!(rest.receipts.len(), 2);
        drop(received);
        node.close_profile().unwrap();
    }
    #[test]
    fn selected_deleted_body_advances_without_replaying_payload() {
        let dir = tempfile::tempdir().unwrap();
        let node = open(dir.path());
        let mut received = ReceivedStore::from_profile(
            node.storage.clone(),
            &dir.path().join("received.log"),
            "pw",
        )
        .unwrap();
        let cursor = node.reception_resume(ReceptionConsumer::Desktop).unwrap();
        receive(&node, &mut received, 1);
        receive(&node, &mut received, 2);
        received
            .remove_where(|entry| entry.event_id == EventId::new([1; 32]))
            .unwrap();
        let page = node
            .reception_page(cursor, None, ReceptionLimit::default())
            .unwrap();
        assert!(page.complete);
        assert_eq!(page.receipts.len(), 1);
        assert_eq!(page.next.sequence, 2);
        drop(received);
        node.close_profile().unwrap();
    }
}
