//! Durable direct and account-addressed DM enqueue and compatibility send APIs.
use super::conversation::{account_conversation_id, dm_conversation_id};
use super::delivery_store::{DeliveryDestination, DeliveryTransaction, OutgoingDelivery};
use super::node::{now_millis, random_msg_id};
use super::sentlog::SentEntry;
use super::*;
use crate::discovery::roster::PeerRecord;
use crate::eventlog::event::{Author, Event, EventId, EventKind};

impl Node {
    pub(in crate::node) fn account_fanout_targets(
        &self,
        target_account_id: &str,
    ) -> Vec<PeerRecord> {
        let my_account = self.account_id();
        let me = self.user_id();
        let mut seen = std::collections::HashSet::new();
        self.routing_peers()
            .into_iter()
            .filter(|p| {
                p.account_id.as_deref() == Some(target_account_id)
                    || p.account_id.as_deref() == Some(my_account.as_str())
            })
            .filter(|p| p.public.user_id() != me)
            .filter(|p| seen.insert(p.public.user_id()))
            .collect()
    }

    pub async fn send_dm(&self, recipient: &str, text: &[u8]) -> Result<(), NodeError> {
        self.send_dm_reply(recipient, text, None).await
    }

    /// Preserves the SDK return type. Durable acceptance succeeds even offline.
    pub async fn send_dm_reply(
        &self,
        recipient: &str,
        text: &[u8],
        reply_to: Option<EventId>,
    ) -> Result<(), NodeError> {
        let peer = self
            .routing_peer(recipient)
            .ok_or_else(|| NodeError::UnknownPeer(recipient.to_string()))?;
        self.initiate_device(&peer.public)
            .await
            .map_err(|e| NodeError::Log(crate::eventlog::LogError::Io(e)))?;
        let id = self.enqueue_encoded(
            vec![peer],
            None,
            MessageBody::new(text.to_vec(), reply_to).encode(),
            None,
        )?;
        self.flush_delivery(id).await;
        Ok(())
    }

    /// Accept one logical account message durably without waiting for TCP.
    pub async fn enqueue_to_account(
        &self,
        target: &str,
        text: &[u8],
        reply_to: Option<EventId>,
    ) -> Result<EventId, NodeError> {
        self.enqueue_to_account_if(target, text, reply_to, |accept| accept())
            .await
    }

    /// Calls `authorize` initially, around the synchronous local privacy grant,
    /// and at final WAL acceptance.
    /// For each successful invocation, the callback must execute the supplied
    /// synchronous operation exactly once and propagate its result. The final
    /// operation is single-use; retaining a synchronous owner guard through it
    /// makes authorization and durable acceptance atomic.
    pub async fn enqueue_to_account_if(
        &self,
        target: &str,
        text: &[u8],
        reply_to: Option<EventId>,
        authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<EventId, NodeError> {
        self.enqueue_account_inner(
            target,
            MessageBody::new(text.to_vec(), reply_to).encode(),
            authorize,
        )
        .await
    }

    pub async fn enqueue_sticker_to_account(
        &self,
        target: &str,
        sticker_id: &str,
        fallback: &[u8],
    ) -> Result<EventId, NodeError> {
        self.enqueue_sticker_to_account_if(target, sticker_id, fallback, |accept| accept())
            .await
    }

    /// Calls `authorize` initially, around the synchronous local privacy grant,
    /// and at final WAL acceptance.
    /// For each successful invocation, execute the supplied synchronous operation
    /// exactly once and propagate its result; the final WAL operation is single-use.
    pub async fn enqueue_sticker_to_account_if(
        &self,
        target: &str,
        sticker_id: &str,
        fallback: &[u8],
        authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<EventId, NodeError> {
        self.enqueue_account_inner(
            target,
            MessageBody::sticker(sticker_id.to_owned(), fallback.to_vec()).encode(),
            authorize,
        )
        .await
    }

    pub async fn send_to_account(
        &self,
        target: &str,
        text: &[u8],
        reply_to: Option<EventId>,
    ) -> Result<(), NodeError> {
        let id = self.enqueue_to_account(target, text, reply_to).await?;
        self.flush_delivery(id).await;
        Ok(())
    }

    pub async fn send_sticker_to_account(
        &self,
        target: &str,
        sticker_id: &str,
        fallback: &[u8],
    ) -> Result<(), NodeError> {
        let id = self
            .enqueue_sticker_to_account(target, sticker_id, fallback)
            .await?;
        self.flush_delivery(id).await;
        Ok(())
    }

    async fn enqueue_account_inner(
        &self,
        target: &str,
        inner: Vec<u8>,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<EventId, NodeError> {
        authorize(&mut || Ok(()))?;
        let dests = self.account_fanout_targets(target);
        if !dests
            .iter()
            .any(|p| p.account_id.as_deref() == Some(target))
        {
            return Err(NodeError::UnknownPeer(target.to_string()));
        }
        self.initiate_contact_locally_if(target, &mut authorize)
            .await?;
        let msg_id = random_msg_id();
        let envelope =
            DmEnvelope::new(self.account_id(), target.to_owned(), msg_id, inner).encode();
        self.enqueue_encoded_if(
            dests,
            Some(target.to_owned()),
            envelope,
            Some(EventId::new(msg_id)),
            &mut authorize,
        )
    }

    fn enqueue_encoded(
        &self,
        dests: Vec<PeerRecord>,
        target: Option<String>,
        plaintext: Vec<u8>,
        logical: Option<EventId>,
    ) -> Result<EventId, NodeError> {
        self.enqueue_encoded_if(dests, target, plaintext, logical, &mut |accept| accept())
    }

    fn enqueue_encoded_if(
        &self,
        dests: Vec<PeerRecord>,
        target: Option<String>,
        plaintext: Vec<u8>,
        logical: Option<EventId>,
        authorize: &mut impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<EventId, NodeError> {
        let mut store = self.delivery.lock().expect("delivery lock not poisoned");
        self.recover_delivery(&mut store).map_err(NodeError::Log)?;
        store
            .check_capacity(dests.len(), plaintext.len() as u64)
            .map_err(NodeError::Log)?;
        let bindings: Vec<_> = dests
            .into_iter()
            .map(|peer| {
                let proof_account = self
                    .historical_author(&peer.public.ed25519_pub)
                    .filter(|p| p.public() == peer.public)
                    .and_then(|p| p.account_id());
                (peer, proof_account)
            })
            .collect();
        let ratchet = self.dm_ratchet.lock().expect("ratchet lock not poisoned");
        let log = self.log.lock().expect("log lock not poisoned");
        let own = self.identity.public();
        let account = self.account_id();
        let author = Author::from_ed25519(own.ed25519_pub);
        let clock = now_millis();
        let mut destinations = Vec::with_capacity(bindings.len());
        let mut prepared = Vec::with_capacity(bindings.len());
        for (peer, proof_account) in bindings {
            let conv = dm_conversation_id(&own, &peer.public);
            let (wire, transition) = ratchet
                .prepare_encrypt(&self.identity, &peer.public, &plaintext)
                .map_err(NodeError::Log)?;
            let (parents, lamport) = log.prepare(&conv);
            let seq = log.version_vector(&conv).get(&author).copied().unwrap_or(0) + 1;
            let event = Event::new(
                &self.identity,
                conv,
                seq,
                parents,
                lamport,
                clock,
                EventKind::Message,
                wire,
            );
            if !super::session::event_fits_frame(&event) {
                return Err(NodeError::InvalidInput(
                    "message exceeds transport frame".into(),
                ));
            }
            let eligible =
                proof_account.is_some() && proof_account.as_deref() != Some(account.as_str());
            destinations.push(DeliveryDestination {
                device: peer.public,
                account: proof_account,
                event,
                receipt_eligible: eligible,
            });
            prepared.push(transition);
        }
        let id = logical.unwrap_or(destinations[0].event.id);
        let conv = target
            .as_ref()
            .map_or(destinations[0].event.conversation_id, |t| {
                account_conversation_id(&account, t)
            });
        let recipient = target.or_else(|| destinations[0].account.clone());
        let seq = if logical.is_some() {
            self.sentlog
                .lock()
                .expect("sent lock not poisoned")
                .entries(&conv)
                .iter()
                .map(|e| e.seq)
                .max()
                .unwrap_or(0)
                + 1
        } else {
            destinations[0].event.seq
        };
        log.sync().map_err(NodeError::Log)?;
        drop(log);
        drop(ratchet);
        let transaction = DeliveryTransaction::Outgoing {
            message: OutgoingDelivery {
                logical_id: id,
                sender_account: account,
                recipient_account: recipient,
                conversation: conv,
                wall_clock: clock,
                destinations,
            },
            sent: SentEntry {
                conversation: conv,
                seq,
                wall_clock: clock,
                plaintext,
            },
            ratchets: prepared,
        };
        // WAL acceptance cannot turn into a failed send inviting duplicates.
        if !self.accept_outgoing_transaction(&mut store, transaction, authorize)? {
            log::warn!("accepted delivery awaits local recovery");
        }
        Ok(id)
    }
}
