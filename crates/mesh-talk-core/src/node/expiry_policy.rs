//! Fixed send-time deadlines, clock policy and local erasure on both sides.

use super::{DmEnvelope, Node, NodeError};
use crate::eventlog::event::EventId;

/// Deadline authenticated inside either an account envelope or direct plaintext.
pub(in crate::node) fn plaintext_deadline(wire: &[u8]) -> Option<u64> {
    DmEnvelope::decode(wire).map_or_else(
        || crate::expiry::deadline(wire),
        |envelope| crate::expiry::deadline(&envelope.body),
    )
}

/// Sender-selected UTC deadline must still be in the future at acceptance.
pub(crate) fn validate_outgoing(deadline: Option<u64>) -> Result<(), NodeError> {
    if let Some(deadline) = deadline {
        let now = super::node::now_millis();
        if deadline <= now {
            return Err(NodeError::InvalidInput("expiration time has passed".into()));
        }
        if deadline > now.saturating_add(30 * 24 * 60 * 60 * 1000) {
            return Err(NodeError::InvalidInput(
                "expiration time is too far away".into(),
            ));
        }
    }
    Ok(())
}

impl Node {
    /// A backward clock change cannot make an observed message live forever.
    pub(in crate::node) fn recipient_expired(
        &self,
        wire: &[u8],
        id: EventId,
        sent_at_ms: u64,
    ) -> bool {
        let Some(deadline) = crate::expiry::deadline(wire) else {
            return false;
        };
        if deadline <= self.expiry_now() {
            return true;
        }
        let lifetime = deadline
            .saturating_sub(sent_at_ms)
            .min(30 * 24 * 60 * 60 * 1000);
        let mut seen = self
            .expiry_first_seen
            .lock()
            .expect("expiry mutex not poisoned");
        seen.entry(id)
            .or_insert_with(std::time::Instant::now)
            .elapsed()
            >= std::time::Duration::from_millis(lifetime)
    }
    pub(in crate::node) fn expired_received_file(
        &self,
        file_conv: crate::eventlog::event::ConversationId,
    ) -> bool {
        let files = self
            .received_files
            .lock()
            .expect("files mutex not poisoned");
        files.conversations().into_iter().any(|conv| {
            files.entries_ref(&conv).iter().any(|row| {
                self.recipient_expired(&row.plaintext, row.event_id, row.wall_clock)
                    && super::files::validated_manifest(&row.plaintext)
                        .is_some_and(|manifest| manifest.file_conv() == file_conv)
            })
        })
    }
    /// The process anchor prevents setting the wall clock backward from extending a deadline.
    pub(in crate::node) fn expiry_now(&self) -> u64 {
        let (start, wall) = self.expiry_clock_start;
        super::node::now_millis().max(wall.saturating_add(start.elapsed().as_millis() as u64))
    }

    pub(in crate::node) fn prune_expired_received(&self) -> Result<usize, NodeError> {
        let removed = self.prune_received_deadlines(false)?;
        self.prune_favorite_files()?;
        Ok(removed)
    }

    // After restart the previous monotonic anchor is gone. Fail closed for recipient
    // copies with deadlines: erasing early is preferable to extending their lifetime.
    pub(in crate::node) fn prune_expired_received_on_open(&self) -> Result<usize, NodeError> {
        self.prune_received_deadlines(true)
    }

    fn prune_received_deadlines(&self, restart: bool) -> Result<usize, NodeError> {
        let own_account = self.account_id();
        let own_devices: std::collections::HashSet<_> = self
            .linked_devices()
            .into_iter()
            .map(|device| device.public.user_id())
            .collect();
        let mut targets = Vec::new();
        {
            let received = self.received.lock().expect("received mutex not poisoned");
            for conv in received.conversations() {
                for row in received.entries_ref(&conv) {
                    let envelope = DmEnvelope::decode(&row.plaintext);
                    let inner = envelope
                        .as_ref()
                        .map_or(row.plaintext.as_slice(), |env| env.body.as_slice());
                    let recipient = row.from != own_account && !own_devices.contains(&row.from);
                    if crate::expiry::deadline(inner).is_some()
                        && ((restart && recipient)
                            || self.recipient_expired(inner, row.event_id, row.wall_clock))
                    {
                        let account = envelope.is_some();
                        let target = envelope.map_or(row.event_id, |env| EventId::new(env.msg_id));
                        targets.push((conv, target, account, row.event_id));
                    }
                }
            }
        }
        {
            let files = self
                .received_files
                .lock()
                .expect("files mutex not poisoned");
            for conv in files.conversations() {
                for row in files.entries_ref(&conv) {
                    let recipient = row.from != own_account && !own_devices.contains(&row.from);
                    if crate::expiry::deadline(&row.plaintext).is_some()
                        && ((restart && recipient)
                            || self.recipient_expired(&row.plaintext, row.event_id, row.wall_clock))
                    {
                        targets.push((conv, row.event_id, true, row.event_id));
                    }
                }
            }
        }
        // Snapshot only conversations with due sender rows. Resolve legacy sequence
        // identities after releasing the sidecar lock (delivery uses the reverse order).
        let due_conversations = {
            let sent = self.sentlog.lock().expect("sent mutex not poisoned");
            sent.conversations()
                .into_iter()
                .filter(|conv| {
                    sent.entries_ref(conv).iter().any(|row| {
                        plaintext_deadline(&row.plaintext)
                            .is_some_and(|deadline| deadline <= self.expiry_now())
                    })
                })
                .collect::<Vec<_>>()
        };
        for conv in due_conversations {
            for (event_id, row) in self.raw_sent_history(conv) {
                if plaintext_deadline(&row.plaintext)
                    .is_some_and(|deadline| deadline <= self.expiry_now())
                {
                    let envelope = DmEnvelope::decode(&row.plaintext);
                    let account = envelope.is_some();
                    let target = envelope.map_or(event_id, |env| EventId::new(env.msg_id));
                    targets.push((conv, target, account, event_id));
                }
            }
        }
        {
            let delivery = self.delivery.lock().expect("delivery lock not poisoned");
            for transaction in delivery.pending_transactions() {
                match transaction.body() {
                    super::delivery_store::DeliveryTransaction::Outgoing {
                        message, sent, ..
                    } if plaintext_deadline(&sent.plaintext)
                        .is_some_and(|deadline| deadline <= self.expiry_now()) =>
                    {
                        targets.push((
                            message.conversation,
                            message.logical_id,
                            DmEnvelope::decode(&sent.plaintext).is_some(),
                            message.logical_id,
                        ));
                    }
                    super::delivery_store::DeliveryTransaction::OutgoingManifest {
                        message,
                        received,
                        ..
                    } if plaintext_deadline(&received.plaintext)
                        .is_some_and(|deadline| deadline <= self.expiry_now()) =>
                    {
                        targets.push((
                            message.conversation,
                            message.logical_id,
                            true,
                            message.logical_id,
                        ));
                    }
                    _ => {}
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        targets.retain(|target| seen.insert(*target));
        let mut removed = 0;
        for (conv, id, account, event_id) in targets {
            removed += self.delete_message(conv, id, account)?;
            self.expiry_first_seen
                .lock()
                .expect("expiry mutex not poisoned")
                .remove(&event_id);
        }
        if removed > 0 {
            self.note_account_history_change();
        }
        Ok(removed)
    }
}
