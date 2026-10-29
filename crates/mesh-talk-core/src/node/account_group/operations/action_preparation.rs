//! Source selection and local metadata snapshots for exact message actions.
use super::*;
use crate::{
    eventlog::{Author, EventKind},
    node::message_actions::{MessageActionInput, MessageActionTarget},
};

pub(in crate::node) fn persist_action_recall_completion(
    reference: &journal::OperationReference,
    mark: Option<&crate::node::recall::RecallMark>,
    recalls: &mut crate::node::recall::RecallLog,
) -> Result<(), crate::eventlog::LogError> {
    match (reference.kind, mark) {
        (AccountGroupOperationKind::Recall, Some(mark))
            if mark.conversation == reference.group
                && journal::operation_digest(
                    reference.group,
                    &AccountGroupOperation::Recall {
                        target: mark.target,
                    },
                ) == reference.digest =>
        {
            recalls.record(mark.conversation, mark.target, mark.wall_clock)
        }
        (AccountGroupOperationKind::Recall, _) | (_, Some(_)) => Err(
            crate::eventlog::LogError::CorruptFile("recall completion binding changed".into()),
        ),
        (_, None) => Ok(()),
    }
}

pub(super) fn action_target_expired(target: Option<&MessageActionTarget>, now: u64) -> bool {
    target.is_some_and(|target| {
        target.expires_at_ms.is_some_and(|deadline| deadline <= now)
            || now.saturating_sub(target.original.wall_clock)
                > crate::node::recall::RECALL_WINDOW_MS
    })
}

impl Node {
    /// A private original send receipt retains full source provenance even when
    /// the requester intentionally does not download public history.
    pub(super) fn account_group_original_message_author(
        &self,
        group: ConversationId,
        target: EventId,
    ) -> Option<Author> {
        if let Some(event) = self.log.lock().expect("log mutex").get(&target).cloned() {
            return (event.conversation_id == group
                && matches!(event.kind, EventKind::Message | EventKind::FileManifest))
            .then_some(event.author);
        }
        self.group_operations
            .lock()
            .expect("operation journal mutex")
            .entries
            .values()
            .find(|entry| {
                entry.reference.group == group
                    && entry.status.phase == AccountGroupOperationPhase::Committed
                    && entry.status.primary_event == Some(target)
                    && matches!(
                        entry.reference.kind,
                        AccountGroupOperationKind::Text
                            | AccountGroupOperationKind::Sticker
                            | AccountGroupOperationKind::File
                    )
            })
            .map(|entry| Author::from_ed25519(entry.reference.executor.device.ed25519_pub))
    }

    /// Snapshot sidecars before the allocation log guard. The pure producer
    /// rechecks that this exact original event remains in its supplied log view.
    pub(super) fn account_group_action_target(
        &self,
        group: ConversationId,
        input: &MessageActionInput,
    ) -> Result<Option<MessageActionTarget>, NodeError> {
        let MessageActionInput::Recall { target } = input else {
            return Ok(None);
        };
        let original = self
            .log
            .lock()
            .expect("log mutex")
            .get(target)
            .cloned()
            .ok_or_else(|| NodeError::Channel("recall original event is reconciling".into()))?;
        if original.conversation_id != group {
            return Err(NodeError::InvalidInput(
                "recall target belongs to another group".into(),
            ));
        }
        let file = self
            .received_files
            .lock()
            .expect("received files mutex")
            .entries_ref(&group)
            .iter()
            .find(|row| row.event_id == *target)
            .cloned();
        let sent = self
            .sentlog
            .lock()
            .expect("sent log mutex")
            .entries_ref(&group)
            .iter()
            .find(|row| row.seq == original.seq)
            .cloned();
        let received = self
            .received
            .lock()
            .expect("received log mutex")
            .entries_ref(&group)
            .iter()
            .find(|row| row.event_id == *target)
            .cloned();
        let plaintext = file
            .as_ref()
            .map(|row| row.plaintext.as_slice())
            .or_else(|| sent.as_ref().map(|row| row.plaintext.as_slice()))
            .or_else(|| received.as_ref().map(|row| row.plaintext.as_slice()))
            .ok_or_else(|| {
                NodeError::InvalidInput("recall source metadata is unavailable".into())
            })?;
        Ok(Some(MessageActionTarget {
            expires_at_ms: crate::expiry::deadline(plaintext),
            protected_file: file
                .as_ref()
                .and_then(|row| crate::node::files::validated_manifest(&row.plaintext))
                .is_some_and(|manifest| manifest.protection().is_some()),
            original,
        }))
    }
}
