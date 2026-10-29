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
        self.recipient_metadata_expired(deadline, id, sent_at_ms)
    }
    pub(in crate::node) fn recipient_metadata_expired(
        &self,
        deadline: u64,
        id: EventId,
        sent_at_ms: u64,
    ) -> bool {
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
    ) -> Result<bool, crate::eventlog::LogError> {
        let files = self.received_files.lock().expect("files mutex");
        let mut after = None;
        loop {
            let page = files.file_headers(
                &file_conv,
                after,
                super::transcript_repository::TranscriptPageLimit::default(),
            )?;
            if page.rows.is_empty() {
                return Ok(false);
            }
            for h in page.rows {
                after = Some(h.ordinal);
                let Some(deadline) = h.deadline else {
                    continue;
                };
                let id = EventId::new(h.key.as_slice().try_into().map_err(|_| {
                    crate::eventlog::LogError::CorruptFile("invalid file event key".into())
                })?);
                if self.recipient_metadata_expired(deadline, id, h.wall_clock) {
                    return Ok(true);
                }
            }
        }
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
        let mut removed = 0;
        for file in [false, true] {
            let mut after = None;
            loop {
                let page = if file {
                    self.received_files
                        .lock()
                        .expect("files mutex")
                        .deadline_page(None, after, 128)?
                } else {
                    self.received
                        .lock()
                        .expect("received mutex")
                        .deadline_page(None, after, 128)?
                };
                if page.is_empty() {
                    break;
                }
                for row in page {
                    after = Some(row.cursor);
                    let event_id = row.event_id.ok_or_else(|| {
                        crate::eventlog::LogError::CorruptFile(
                            "missing received deadline identity".into(),
                        )
                    })?;
                    let recipient = row.sender != own_account && !own_devices.contains(&row.sender);
                    if (restart && recipient)
                        || self.recipient_metadata_expired(
                            row.cursor.deadline,
                            event_id,
                            row.wall_clock,
                        )
                    {
                        removed += self.delete_message(
                            row.conversation,
                            row.logical_id.unwrap_or(event_id),
                            file || row.logical_id.is_some(),
                        )?;
                        self.expiry_first_seen
                            .lock()
                            .expect("expiry mutex")
                            .remove(&event_id);
                    }
                }
            }
        }
        let mut after = None;
        loop {
            let page = self.sentlog.lock().expect("sent mutex").deadline_page(
                Some(self.expiry_now()),
                after,
                128,
            )?;
            if page.is_empty() {
                break;
            }
            for row in page {
                after = Some(row.cursor);
                let id = if let Some(id) = row.logical_id {
                    id
                } else {
                    let header = self
                        .sentlog
                        .lock()
                        .expect("sent mutex")
                        .header_by_ordinal(row.cursor.ordinal)?
                        .ok_or_else(|| {
                            crate::eventlog::LogError::CorruptFile(
                                "selected expiry row disappeared".into(),
                            )
                        })?;
                    self.sent_header_identity(&header)?
                };
                removed += self.delete_message(row.conversation, id, row.logical_id.is_some())?;
            }
        }
        let mut targets = Vec::new();
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
