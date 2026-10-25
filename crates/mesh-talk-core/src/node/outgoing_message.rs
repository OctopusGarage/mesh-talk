//! Retry-safe message acceptance shared by local integrations and ordinary sends.
use super::delivery_store::{DeliveryTransaction, RequestRecord};
use super::node::now_millis;
use super::*;
use crate::eventlog::event::{Author, ConversationId, Event, EventId, EventKind};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageDestination {
    Account(String),
    Channel(ConversationId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutgoingMessageRequest {
    pub request_id: [u8; 32],
    pub destination: MessageDestination,
    pub text: Vec<u8>,
    pub reply_to: Option<EventId>,
    pub expires_at_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageAcceptance {
    pub message_id: EventId,
    pub duplicate: bool,
}

impl OutgoingMessageRequest {
    fn digest(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let content = bincode::serialize(&(
            &self.destination,
            &self.text,
            self.reply_to,
            self.expires_at_ms,
        ))
        .expect("message request encoding");
        Sha256::digest(content).into()
    }
}

impl Node {
    /// The callback must run the supplied synchronous operation exactly once while
    /// retaining the host's authorization guard, and propagate its error.
    pub async fn enqueue_message_once_if(
        &self,
        request: &OutgoingMessageRequest,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<MessageAcceptance, NodeError> {
        let _serial = self.outgoing_requests_gate.lock().await;
        authorize(&mut || Ok(()))?;
        let digest = request.digest();
        let existing = self
            .delivery
            .lock()
            .expect("delivery lock")
            .request(&request.request_id)
            .cloned();
        if let Some(existing) = existing {
            let mut acceptance = None;
            authorize(&mut || {
                if existing.digest != digest {
                    return Err(NodeError::RequestConflict);
                }
                acceptance = Some(MessageAcceptance {
                    message_id: existing.message_id,
                    duplicate: true,
                });
                Ok(())
            })?;
            return acceptance.ok_or_else(|| {
                NodeError::Authorization("authorization did not execute lookup".into())
            });
        }
        if let Some(reply_to) = request.reply_to
            && !self.integration_reply_retained(&request.destination, reply_to)
        {
            return Err(NodeError::InvalidInput(
                "reply target is not retained within the most recent 500 records per destination history".into(),
            ));
        }
        super::expiry_policy::validate_outgoing(request.expires_at_ms)?;
        let wrapped = crate::expiry::wrap(
            &MessageBody::new(request.text.clone(), request.reply_to).encode(),
            request.expires_at_ms,
        );
        let record = RequestRecord {
            request_id: request.request_id,
            digest,
            destination: request.destination.clone(),
            message_id: EventId::new([0; 32]),
            accepted_at_ms: now_millis(),
        };
        let message_id = match &request.destination {
            MessageDestination::Account(target) => {
                self.enqueue_account_inner(target, wrapped, Some(record), &mut authorize)
                    .await?
            }
            MessageDestination::Channel(channel) => {
                self.enqueue_channel_body_if(*channel, wrapped, Some(record), &mut authorize)?
            }
        };
        Ok(MessageAcceptance {
            message_id,
            duplicate: false,
        })
    }

    /// Bounded admission lookup: at most 500 records per plaintext sidecar.
    /// This validates a retained destination reference, not read permission or
    /// current content visibility (retained slots can include recalls/own expiry).
    /// Older references are deliberately rejected rather than materializing and
    /// sorting an unbounded conversation on the integration request path.
    fn integration_reply_retained(
        &self,
        destination: &MessageDestination,
        reply_to: EventId,
    ) -> bool {
        const RECENT_RECORDS: usize = 500;
        let conversation = match destination {
            MessageDestination::Account(account) => {
                super::conversation::account_conversation_id(&self.account_id(), account)
            }
            MessageDestination::Channel(channel) => *channel,
        };
        let channel_seq = match destination {
            MessageDestination::Channel(_) => self
                .log
                .lock()
                .expect("log lock")
                .get(&reply_to)
                .filter(|event| {
                    event.conversation_id == conversation
                        && event.author.user_id() == self.user_id()
                })
                .map(|event| event.seq),
            MessageDestination::Account(_) => None,
        };
        let sent = self.sentlog.lock().expect("sent lock");
        let own_retained = sent
            .entries_ref(&conversation)
            .iter()
            .rev()
            .take(RECENT_RECORDS)
            .any(|entry| match destination {
                MessageDestination::Account(_) => DmEnvelope::decode(&entry.plaintext)
                    .is_some_and(|envelope| EventId::new(envelope.msg_id) == reply_to),
                MessageDestination::Channel(_) => Some(entry.seq) == channel_seq,
            });
        drop(sent);
        if own_retained {
            return true;
        }
        let received = self.received.lock().expect("received lock");
        let incoming_retained = received
            .entries_ref(&conversation)
            .iter()
            .rev()
            .take(RECENT_RECORDS)
            .any(|entry| {
                let envelope = matches!(destination, MessageDestination::Account(_))
                    .then(|| DmEnvelope::decode(&entry.plaintext))
                    .flatten();
                let id = envelope
                    .as_ref()
                    .map_or(entry.event_id, |envelope| EventId::new(envelope.msg_id));
                let plaintext = envelope
                    .as_ref()
                    .map_or(entry.plaintext.as_slice(), |envelope| {
                        envelope.body.as_slice()
                    });
                id == reply_to
                    && !self.recipient_expired(plaintext, entry.event_id, entry.wall_clock)
            });
        drop(received);
        if incoming_retained {
            return true;
        }
        self.received_files
            .lock()
            .expect("file history lock")
            .entries_ref(&conversation)
            .iter()
            .rev()
            .take(RECENT_RECORDS)
            .any(|entry| {
                entry.event_id == reply_to
                    && !self.recipient_expired(&entry.plaintext, entry.event_id, entry.wall_clock)
            })
    }

    pub fn message_request_status(
        &self,
        request_id: &[u8; 32],
    ) -> Result<Option<MessageAcceptance>, NodeError> {
        Ok(self
            .delivery
            .lock()
            .expect("delivery lock")
            .request(request_id)
            .map(|r| MessageAcceptance {
                message_id: r.message_id,
                duplicate: true,
            }))
    }

    pub fn message_request_destination(
        &self,
        request_id: &[u8; 32],
    ) -> Result<Option<MessageDestination>, NodeError> {
        Ok(self
            .delivery
            .lock()
            .expect("delivery lock")
            .request(request_id)
            .map(|r| r.destination.clone()))
    }

    pub async fn flush_accepted_message(
        &self,
        destination: &MessageDestination,
        message_id: EventId,
    ) {
        match destination {
            MessageDestination::Account(_) => self.flush_delivery(message_id).await,
            MessageDestination::Channel(channel) => {
                let ready = {
                    let mut store = self.delivery.lock().expect("delivery lock");
                    self.recover_delivery(&mut store).is_ok()
                };
                if !ready {
                    return;
                }
                let members = self
                    .channels
                    .lock()
                    .expect("channels lock")
                    .state(channel)
                    .map(|s| s.members().to_vec());
                if let Some(members) = members {
                    self.distribute_channel_message(*channel, &members, message_id)
                        .await;
                }
            }
        }
    }

    pub(in crate::node) fn enqueue_channel_body_if(
        &self,
        channel: ConversationId,
        wrapped: Vec<u8>,
        request: Option<RequestRecord>,
        authorize: &mut impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<EventId, NodeError> {
        self.enqueue_channel_payload_if(channel, wrapped, EventKind::Message, request, authorize)
    }

    /// Prepare the existing sender-key batch while the caller holds membership,
    /// delivery, current channel state and event-log guards. The caller validates
    /// its full source projection and actual channel epoch before entering here.
    /// This syncs retained causal parents but neither appends nor publishes events.
    pub(in crate::node) fn prepare_channel_outgoing_locked(
        &self,
        channel: ConversationId,
        wrapped: Vec<u8>,
        kind: EventKind,
        log: &crate::eventlog::PersistentEventLog,
        state: &crate::channel::ChannelState,
    ) -> Result<(DeliveryTransaction, EventId), NodeError> {
        let (payload, rotation, senders) = state
            .prepare_sender_message(&wrapped)
            .map_err(|e| NodeError::Channel(e.to_string()))?;
        let author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let (mut parents, mut lamport) = log.prepare(&channel).map_err(NodeError::Log)?;
        let mut seq = log
            .next_sequence(&channel, &author)
            .map_err(NodeError::Log)?;
        let clock = now_millis();
        let mut events = Vec::new();
        if let Some(distribution) = rotation {
            let sealed = super::channel::seal_keys_for(
                &self.identity,
                &state.recipient_members(),
                &distribution,
                state.epoch(),
            )
            .map_err(|e| NodeError::Channel(e.to_string()))?;
            let event = Event::new(
                &self.identity,
                channel,
                seq,
                parents,
                lamport,
                clock,
                EventKind::KeyRotation,
                sealed.encode(),
            );
            parents = vec![event.id];
            lamport = lamport
                .checked_add(1)
                .ok_or(NodeError::Log(crate::eventlog::LogError::ClockExhausted))?;
            seq = seq
                .checked_add(1)
                .ok_or(NodeError::Log(crate::eventlog::LogError::SequenceExhausted))?;
            events.push(event);
        }
        let event = Event::new(
            &self.identity,
            channel,
            seq,
            parents,
            lamport,
            clock,
            kind,
            payload,
        );
        if !super::session::event_fits_frame(&event) {
            return Err(NodeError::InvalidInput(
                "message exceeds transport frame".into(),
            ));
        }
        let id = event.id;
        events.push(event);
        log.sync().map_err(NodeError::Log)?;
        let transaction = DeliveryTransaction::ChannelOutgoing {
            events,
            sent: super::sentlog::SentEntry {
                conversation: channel,
                seq,
                wall_clock: clock,
                plaintext: wrapped,
            },
            senders,
        };
        Ok((transaction, id))
    }

    pub(in crate::node) fn enqueue_channel_payload_if(
        &self,
        channel: ConversationId,
        wrapped: Vec<u8>,
        kind: EventKind,
        request: Option<RequestRecord>,
        authorize: &mut impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<EventId, NodeError> {
        let mut result = None;
        let mut wrapped = Some(wrapped);
        let mut request = request;
        authorize(&mut || {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            let mut store = self.delivery.lock().expect("delivery lock");
            let wrapped = wrapped
                .take()
                .ok_or_else(|| NodeError::InvalidInput("accept is single-use".into()))?;
            self.recover_delivery(&mut store).map_err(NodeError::Log)?;
            self.process_channel(channel);
            let book = self.channels.lock().expect("channels lock");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
            if !self.channel_identity_is_current(state)
                || state.pending_invite()
                || state.is_dissolved()
            {
                return Err(NodeError::Channel("not an active channel member".into()));
            }
            let log = self.log.lock().expect("log lock");
            let (mut transaction, id) =
                self.prepare_channel_outgoing_locked(channel, wrapped, kind, &log, state)?;
            drop(log);
            if let Some(mut record) = request.take() {
                record.message_id = id;
                record.accepted_at_ms = now_millis();
                transaction = DeliveryTransaction::Requested {
                    request: record,
                    transaction: Box::new(transaction),
                };
            }
            store.begin(transaction).map_err(NodeError::Log)?;
            drop(book);
            result = Some(id);
            Ok(())
        })?;
        let id = result.ok_or_else(|| {
            NodeError::Authorization("authorization did not execute acceptance".into())
        })?;
        let mut store = self.delivery.lock().expect("delivery lock");
        if self.recover_delivery(&mut store).is_err() {
            log::warn!("accepted channel message awaits local recovery");
        }
        self.delivery_notify.notify_one();
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::ChannelMeta;
    use crate::discovery::Announce;
    use crate::eventlog::LogError;
    use crate::identity::{account::Account, device::DeviceIdentity};

    #[tokio::test]
    async fn two_event_rotation_clock_exhaustion_leaves_wal_and_sender_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let peer = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "peer",
            9,
        );
        let (node, _, _) = super::super::test_support::node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &peer,
        );
        let channel = crate::channel::new_channel_id();
        let meta = ChannelMeta {
            name: "clock boundary".into(),
            members: vec![node.identity.public()],
            epoch: 0,
            owner: node.user_id(),
        };
        // A valid genesis without a distributed sender key forces the next send
        // to prepare both a KeyRotation and a Message. One clock slot remains.
        let genesis = Event::new(
            &node.identity,
            channel,
            1,
            vec![],
            u64::MAX - 1,
            0,
            EventKind::MembershipChange,
            meta.encode(),
        );
        node.log
            .lock()
            .unwrap()
            .append_durable(genesis.clone())
            .unwrap();
        node.process_channel(channel);
        let before_senders = node
            .channels
            .lock()
            .unwrap()
            .state(&channel)
            .unwrap()
            .export_my_senders();
        assert!(before_senders.is_empty());
        assert!(
            node.delivery
                .lock()
                .unwrap()
                .pending_transactions()
                .is_empty()
        );
        let snapshot = || {
            [
                "events.log",
                "sent.log",
                "delivery-transactions.log",
                "delivery-outbox.log",
                "channel.senders",
            ]
            .map(|name| match std::fs::read(dir.path().join(name)) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => panic!("cannot snapshot {name}: {error}"),
            })
        };
        let before_files = snapshot();

        let result = node.enqueue_channel_body_if(
            channel,
            b"must not commit".to_vec(),
            None,
            &mut |commit| commit(),
        );

        assert!(matches!(
            result,
            Err(NodeError::Log(LogError::ClockExhausted))
        ));
        assert_eq!(snapshot(), before_files);
        assert!(
            node.delivery
                .lock()
                .unwrap()
                .pending_transactions()
                .is_empty()
        );
        assert_eq!(
            node.channels
                .lock()
                .unwrap()
                .state(&channel)
                .unwrap()
                .export_my_senders(),
            before_senders,
        );
        let log = node.log.lock().unwrap();
        assert_eq!(log.heads(&channel), vec![genesis.id]);
        assert_eq!(log.events(&channel).len(), 1);
        assert_eq!(log.prepare(&channel).unwrap().1, u64::MAX);
    }
}
